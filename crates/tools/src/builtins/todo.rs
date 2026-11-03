//! `todo_write`: the session's plan, as a board with one half per author - the model, the
//! operator, a parent session, the merge queue - where a row is addressed by its own words and
//! nothing removes one. The harness persists the board and watches its version.

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
    /// The operator's rows, kept beside the model's: one board with two halves, and `snapshot`
    /// is their union, so every reader picks them up without knowing they exist. Separate from
    /// `todos` because a write to that vector is scoped to the model's own rows.
    operator: Mutex<Vec<TodoItem>>,
    /// A parent session's rows on this board, kept apart from the model's own: the whole-list
    /// contract is about your own rows, so a parent's write is scoped to its authorship
    /// ([`TodoBoard::upsert_parent`]) and never reaches the child's plan or the operator's rows.
    parent: Mutex<Vec<TodoItem>>,
    /// The merge queue's rows: the only half that is a view, replaced from the store on every
    /// pass ([`TodoBoard::set_queue`]) rather than accumulated. An entry's state lives in the
    /// store's own tables, so anything that counts the queue counts those and not this.
    queue: Mutex<Vec<TodoItem>>,
    version: AtomicU64,
}

impl TodoBoard {
    /// Start with a list, split by author: a resumed session's rows must land back in the half
    /// that owns them, or the model's own write would take the operator's rows for its own.
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

    /// The operator's half, replaced wholesale: a head sends its whole list on every change, and
    /// a delta protocol would be a second source of truth. The title cap is not checked here - a
    /// replace carries rows written before the cap, and one of them must stay closeable.
    pub fn set_operator(&self, todos: Vec<TodoItem>) -> u64 {
        *self.operator.lock().unwrap_or_else(|e| e.into_inner()) = todos;
        self.version.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Move the state of rows on the model's half, the operator's, or both, all or nothing:
    /// every quote resolves before any is applied, because a partial write leaves the two halves
    /// drifted. It sets state and never membership, and the version moves only when a row changed.
    pub fn set_states_on_both_halves(
        &self,
        mine: &[(String, TodoStatus)],
        theirs: &[(String, TodoStatus)],
    ) -> Result<(usize, usize), String> {
        let mut model = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        let mut operator = self.operator.lock().unwrap_or_else(|e| e.into_inner());
        // Resolve every quote on BOTH halves before either is touched - see ALL OR NOTHING.
        let my_plan = resolve_plan(&model, mine, "your own")?;
        let their_plan = resolve_plan(&operator, theirs, "the operator's")?;
        let mine_moved = apply_states(&mut model, my_plan);
        let theirs_moved = apply_states(&mut operator, their_plan);
        if mine_moved + theirs_moved > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        Ok((mine_moved, theirs_moved))
    }

    /// **The operator's half alone** - the shape a caller that has only their rows to move wants,
    /// and the one this board's own tests and the `operator` field had before `update` existed.
    pub fn set_operator_states(&self, updates: &[(String, TodoStatus)]) -> Result<usize, String> {
        self.set_states_on_both_halves(&[], updates)
            .map(|(_, theirs)| theirs)
    }

    /// The operator's door for a row that is not theirs: `/todo postpone|resume` on a model's
    /// row. State only, never membership, and the model's half only - a parent's row is its
    /// author's to retire, so a head refuses one by name rather than writing a silence.
    pub fn set_states(&self, updates: &[(String, TodoStatus)]) -> Result<usize, String> {
        let mut half = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        // Resolve every update BEFORE any is applied - see `set_operator_states` on ALL OR NOTHING:
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

    /// Attach a wait to a row already on the board, or take its waits off. A wait is an edge on
    /// [`TodoItem::needs`] and nothing else: words and status never move, and detach takes off
    /// the child and job edges while leaving the row edges. The caller checks the handle.
    pub fn set_waits(&self, plan: &[(String, Wait)]) -> Result<(usize, usize), String> {
        let mut model = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        let mut operator = self.operator.lock().unwrap_or_else(|e| e.into_inner());
        // Resolve every quote BEFORE any row moves - see `set_states_on_both_halves` on ALL OR
        // NOTHING.
        let mut resolved: Vec<(bool, usize, &Wait)> = Vec::with_capacity(plan.len());
        for (asked, wait) in plan {
            let want = asked.trim();
            let hits = |half: &[TodoItem]| -> Vec<usize> {
                half.iter()
                    .enumerate()
                    .filter(|(_, t)| t.content.trim() == want)
                    .map(|(i, _)| i)
                    .collect()
            };
            let (mine, theirs) = (hits(&model), hits(&operator));
            match (mine.as_slice(), theirs.as_slice()) {
                ([one], []) => resolved.push((true, *one, wait)),
                ([], [one]) => resolved.push((false, *one, wait)),
                ([], []) => {
                    // **Say WHY a row that is on the board is not reachable**, when it is: a
                    // parent's or the queue's row is there, and *no such row* about it would send
                    // the model hunting for words it already quoted right.
                    let elsewhere = |half: &Mutex<Vec<TodoItem>>| {
                        half.lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .iter()
                            .any(|t| t.content.trim() == want)
                    };
                    if elsewhere(&self.parent) {
                        return Err(format!(
                            "`{want}` is a row your PARENT wrote, and its edges are the parent's: it restates them on its next write, so a wait put on it here would vanish under you. Say in your reply what it is waiting on."
                        ));
                    }
                    if elsewhere(&self.queue) {
                        return Err(format!(
                            "`{want}` is the merge queue's row, written from the store on every pass — nothing on this board can make it wait."
                        ));
                    }
                    let have: Vec<String> = model
                        .iter()
                        .chain(operator.iter())
                        .map(|t| format!("`{}`", t.content.trim()))
                        .collect();
                    return Err(if have.is_empty() {
                        format!(
                            "no row says `{want}` — the board has no rows of yours or the operator's at all."
                        )
                    } else {
                        format!(
                            "no row of yours or the operator's says `{want}`. The rows are: {}",
                            have.join(", ")
                        )
                    });
                }
                _ => {
                    return Err(format!(
                        "`{want}` is {} rows — {} of yours and {} of the operator's — so which one waits is not something this can know. Quote the one you mean exactly.",
                        mine.len() + theirs.len(),
                        mine.len(),
                        theirs.len()
                    ));
                }
            }
        }
        // **A question the operator already answered on this row is not asked again.** Checked
        // before anything moves, for the all-or-nothing rule above: the answer is quoted back, so
        // the model reads what it was told instead of re-asking it.
        for (is_mine, at, wait) in &resolved {
            if let Wait::On(TodoNeed::Operator { question, .. }) = wait {
                let row = if *is_mine {
                    &model[*at]
                } else {
                    &operator[*at]
                };
                if let Some(reply) = answered_reply(row, question) {
                    return Err(format!(
                        "the operator already answered \"{}\" on `{}`: {reply}. Act on that \
                         answer; ask a different question if it raised one.",
                        question.trim(),
                        row.content.trim()
                    ));
                }
            }
        }
        let (mut mine_moved, mut theirs_moved) = (0usize, 0usize);
        for (is_mine, at, wait) in resolved {
            let row = if is_mine {
                &mut model[at]
            } else {
                &mut operator[at]
            };
            let changed = match wait {
                // **A question on a row replaces the question it was asking**, if it was asking
                // one: one row, one thing the operator is asked about it at a time, so a re-worded
                // question is the model correcting itself rather than a second card.
                Wait::On(TodoNeed::Operator { question, options }) => {
                    let same = row.needs.iter().any(|n| {
                        matches!(n, TodoNeed::Operator { question: q, options: o }
                            if q == question && o == options)
                    });
                    if same {
                        false
                    } else {
                        row.needs
                            .retain(|n| !matches!(n, TodoNeed::Operator { .. }));
                        row.needs.push(TodoNeed::Operator {
                            question: question.clone(),
                            options: options.clone(),
                        });
                        true
                    }
                }
                Wait::On(need) => {
                    if row.needs.contains(need) {
                        false
                    } else {
                        row.needs.push(need.clone());
                        true
                    }
                }
                // **And the operator's question comes off with them** - the door the check names
                // for a model that worked the answer out itself. An ANSWER stays: it is the
                // operator's word on the row, not a wait.
                Wait::Detach => {
                    let before = row.needs.len();
                    row.needs.retain(|n| {
                        !matches!(
                            n,
                            TodoNeed::Job { .. }
                                | TodoNeed::Monitor { .. }
                                | TodoNeed::Child { .. }
                                | TodoNeed::Operator { .. }
                        )
                    });
                    row.needs.len() != before
                }
            };
            if changed {
                if is_mine {
                    mine_moved += 1;
                } else {
                    theirs_moved += 1;
                }
            }
        }
        if mine_moved + theirs_moved > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        Ok((mine_moved, theirs_moved))
    }

    /// The operator's half alone, for a caller that needs to tell the two apart.
    pub fn operator_snapshot(&self) -> Vec<TodoItem> {
        self.operator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// **The rows a parent has put on this board, as a sibling reader sees them** - the child
    /// cannot act on them (no field of `todo_write` aims at this half), but the tests and the
    /// daemon-side resolver both need to say what survived.
    pub fn parent_snapshot(&self) -> Vec<TodoItem> {
        self.parent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The merge queue's rows as they stand, for a reader that wants the queue rather than the
    /// board.
    pub fn queue_snapshot(&self) -> Vec<TodoItem> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Set the queue's half rather than merge it: the one writer of that half, and the only door
    /// that takes a row off. A queue row is the queue as seen from a session, not a record of
    /// work somebody chose to do, so the record itself stays in the store's tables.
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

    /// Replace the room's rows, one per item the session holds: the only door that takes one of
    /// the model's own rows away. Retirement goes by the room edge on the row rather than by a
    /// record of what was written, so a board rebuilt from the store still finds them.
    pub fn set_room_rows(&self, rows: Vec<(u64, TodoItem)>) -> usize {
        let n = rows.len();
        let mut half = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        let held: BTreeSet<u64> = rows.iter().map(|(item, _)| *item).collect();
        // **The items this session no longer holds take their rows with them** - found by the edge
        // the pass put on them, which is on the row whether it is quiet or loud.
        let before = half.len();
        half.retain(|t| {
            let gone = crate::builtins::todo::room_items(t)
                .iter()
                .any(|item| !held.contains(item));
            !gone
        });
        if half.len() != before {
            changed = true;
        }
        for (_, row) in rows {
            let want = row.content.trim().to_string();
            // The pass's own edge, and the only one it owns on a row.
            let edge: Vec<TodoNeed> = row
                .needs
                .iter()
                .filter(|n| matches!(n, TodoNeed::Room { .. }))
                .cloned()
                .collect();
            match half.iter_mut().find(|t| t.content.trim() == want) {
                // **Reused**: the words are the key, so this is the same row - the pass's edge and
                // the row's state are what it has to say, and nothing else moves. Any other edge on
                // the row (a `Row`, a `Job`, a question) is the model's or the operator's and stays.
                Some(had) => {
                    let mut needs: Vec<TodoNeed> = had
                        .needs
                        .iter()
                        .filter(|n| !matches!(n, TodoNeed::Room { .. }))
                        .cloned()
                        .collect();
                    needs.extend(edge);
                    if had.needs != needs || had.status != row.status {
                        had.needs = needs;
                        had.status = row.status;
                        changed = true;
                    }
                }
                None => {
                    half.push(row);
                    changed = true;
                }
            }
        }
        if changed {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        n
    }

    /// A parent's write: an upsert scoped to the parent's own authorship, so the child's rows,
    /// the operator's rows and every other author's are untouched and nothing is deleted. A new
    /// row's title is capped here, before anything is pushed.
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
                let needs = keep_answers(&hit.needs, needs);
                if hit.status != *status || hit.needs != needs {
                    hit.status = *status;
                    hit.needs = needs;
                    changed += 1;
                }
            } else {
                half.push(TodoItem {
                    content: content.to_string(),
                    status: *status,
                    by: by.clone(),
                    // A condition is the OPERATOR's own act - a job-conditioned row fires on a
                    // clock the parent does not own, and `[p]`/`postponed` is the operator's own
                    // state besides. A parent's row is plain work, asked for now.
                    when: None,
                    // Its edges are the parent's to write: a plan handed to a child carries the
                    // order too, which is why a child's board has a DAG at all.
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

    /// The model's own rows, as an upsert that never deletes: a matched row moves its state and
    /// its edges, a new text is appended, and a row left out stays where it was. A status the
    /// model cannot spell is the operator's and survives unless the row is completed.
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
                let needs = keep_answers(&hit.needs, needs);
                if hit.status != wanted || hit.needs != needs {
                    hit.status = wanted;
                    hit.needs = needs;
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

    /// **The model's own half alone** - what a call STARTED with, which is what lets the tool name
    /// the rows that call left out (and so did not remove).
    pub fn model_snapshot(&self) -> Vec<TodoItem> {
        self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Clear the condition a row has been reported with, so a daemon that comes back reads the
    /// row as ordinary open work: a condition that never stops firing is indistinguishable from
    /// one that never fired. Only the condition moves.
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

    /// The list as it stands: the union of all four halves, and the one getter everything
    /// downstream reads. The queue's rows come first, so a session working the queue cannot bury
    /// a review under a plan of its own, then the model's, then what it was told.
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

    /// How many writes have landed. A version rather than a dirty flag: two rapid writes that
    /// both need persisting are not lost the way a boolean cleared early loses them.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }
}

/// **The reply the operator gave to this question on this row**, if they have answered it.
fn answered_reply<'a>(row: &'a TodoItem, question: &str) -> Option<&'a str> {
    row.needs.iter().find_map(|n| match n {
        TodoNeed::Answered { question: q, reply } if q.trim() == question.trim() => {
            Some(reply.as_str())
        }
        _ => None,
    })
}

/// A restated row's edges, with the operator's answers kept: an answer is the operator's word,
/// so a restatement that leaves it out does not take it off, and a question already answered on
/// this row is dropped rather than asked again.
fn keep_answers(had: &[TodoNeed], sent: &[TodoNeed]) -> Vec<TodoNeed> {
    let answers: Vec<&TodoNeed> = had
        .iter()
        .filter(|n| matches!(n, TodoNeed::Answered { .. }))
        .collect();
    let answered = |question: &str| {
        answers.iter().any(
            |n| matches!(n, TodoNeed::Answered { question: q, .. } if q.trim() == question.trim()),
        )
    };
    let mut out: Vec<TodoNeed> = sent
        .iter()
        .filter(|n| match n {
            TodoNeed::Operator { question, .. } => !answered(question),
            TodoNeed::Answered { .. } => false,
            _ => true,
        })
        .cloned()
        .collect();
    out.extend(answers.into_iter().cloned());
    out
}

/// **A row's question, found** - the half of [`TodoBoard::answer_operator`] that says whether
/// there is anything to answer, with the row's options so the answer can be checked against them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorAsk {
    pub content: String,
    pub question: String,
    pub options: Vec<String>,
}

impl TodoBoard {
    /// The question this row waits on the operator for, by the row's words and the question's, or
    /// why there is none. Every half the operator can be asked from: the model's, the operator's
    /// own, and a parent's.
    pub fn operator_ask(&self, content: &str, question: &str) -> Result<OperatorAsk, String> {
        let want = content.trim();
        let halves = [&self.todos, &self.operator, &self.parent];
        let mut found_row = false;
        for half in halves {
            let half = half.lock().unwrap_or_else(|e| e.into_inner());
            for row in half.iter().filter(|t| t.content.trim() == want) {
                found_row = true;
                for need in &row.needs {
                    if let TodoNeed::Operator {
                        question: q,
                        options,
                    } = need
                        && q.trim() == question.trim()
                    {
                        return Ok(OperatorAsk {
                            content: row.content.clone(),
                            question: q.clone(),
                            options: options.clone(),
                        });
                    }
                }
                if let Some(reply) = answered_reply(row, question) {
                    return Err(format!(
                        "that question on `{want}` was already answered: {reply}"
                    ));
                }
            }
        }
        Err(if found_row {
            format!(
                "`{want}` is not waiting on that question any more — the model may have worked it \
                 out and taken the wait off, or asked a different one"
            )
        } else {
            format!("no row on the board says `{want}` any more")
        })
    }

    /// Put the operator's answer on the row: the question's edge is replaced in place by
    /// [`TodoNeed::Answered`], and nothing else about the row moves. The version is bumped, so
    /// the harness persists and announces the board at its next flush.
    pub fn answer_operator(
        &self,
        content: &str,
        question: &str,
        reply: &str,
    ) -> Result<(), String> {
        let want = content.trim();
        for half in [&self.todos, &self.operator, &self.parent] {
            let mut half = half.lock().unwrap_or_else(|e| e.into_inner());
            for row in half.iter_mut().filter(|t| t.content.trim() == want) {
                if let Some(i) = row.needs.iter().position(|n| {
                    matches!(n, TodoNeed::Operator { question: q, .. } if q.trim() == question.trim())
                }) {
                    row.needs[i] = TodoNeed::Answered {
                        question: question.trim().to_string(),
                        reply: reply.to_string(),
                    };
                    drop(half);
                    self.version.fetch_add(1, Ordering::SeqCst);
                    return Ok(());
                }
            }
        }
        self.operator_ask(content, question).map(|_| ())
    }
}

/// **One `wait_on` entry, as the board applies it** - see [`TodoBoard::set_waits`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// Wait on this - a [`TodoNeed::Job`] or a [`TodoNeed::Child`], already resolved to the full
    /// handle by the caller. Attaching one the row already has is not a change.
    On(TodoNeed),
    /// Take the row's job and child waits off, and leave its row edges where they are.
    Detach,
}

/// One quote, resolved against one half of the board, and the house rule for a name: exactly
/// one row is the answer, none is refused with the rows that half does hold, and two or more are
/// refused with every candidate named. A guess here would move a row nobody meant.
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

/// Apply a resolved plan to one half. Returns how many rows actually CHANGED - a status set to what
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

/// The open work as a priority queue: `in_progress` first, then `pending`, and within a band the
/// list's own order. Completed and cancelled rows are not work, so they are not in it. This is
/// the order, not the answer - which of these rows can start is the graph's question.
pub fn open_priority(todos: &[TodoItem]) -> Vec<&TodoItem> {
    let mut open: Vec<&TodoItem> = todos
        .iter()
        // A cancelled row is not work at all: the operator struck it off, and the row survives
        // only as history. Postponed is work still owed and stays in the queue.
        .filter(|t| !matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled))
        .collect();
    open.sort_by_key(|t| match t.status {
        TodoStatus::InProgress => 0,
        _ => 1,
    });
    open
}

/// How long a row that waits on a live child stays silent, so the child has room to breathe
/// before the check starts steering its parent at it. Crude on purpose: a rule that watched the
/// child's progress rather than its age is the next thing to think about.
pub const CHILD_ROOM_TO_BREATHE: Duration = Duration::from_secs(30 * 60);

/// How long a child may show nothing before the notice stops calling it working. It decides
/// only which sentence is said, never whether the check speaks. A child inside one long tool call
/// publishes nothing while it runs, so the evidence is printed beside the sentence.
pub const CHILD_QUIET_ENOUGH_TO_LOOK: Duration = Duration::from_secs(2 * 60);

/// A child this daemon is running, as the daemon can see it. The age is what the room to
/// breathe is measured against; the rest is what the check says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunningChild {
    /// How long ago the daemon opened this child's session.
    pub age: Duration,
    /// **How long since anything last happened in it** - the timestamp of the last event on the
    /// child's OWN log, or `None` for a child that has published nothing at all yet.
    pub quiet_for: Option<Duration>,
    /// Whether a turn is in flight in it at this instant, read from the daemon's own record of the
    /// turn rather than `SessionStatus::running`, which is the round's flag and so reads false
    /// through a round's tool calls. Not a verdict: see [`ChildDoing`].
    pub working: bool,
}

/// What the daemon knows about the children a plan waits on: a fact no board can hold, so the
/// caller takes a snapshot and passes it. A child that is not in here is not running, which is an
/// answer rather than a gap - a daemon that restarted has no children left.
#[derive(Debug, Clone, Default)]
pub struct ChildSessions {
    live: BTreeMap<String, RunningChild>,
}

impl ChildSessions {
    /// Nothing running - a reading with no daemon behind it, and the honest one for every board
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

    /// **How long this child has been running**, or `None` when it is not running here - the one
    /// read [`unmet_needs`] and the check's narrowing both take.
    pub fn child(&self, id: &str) -> Option<&RunningChild> {
        self.live.get(id)
    }
}

/// What the daemon knows about the jobs a plan waits on, handed in beside [`ChildSessions`].
/// Liveness is the whole of it, since a job's settlement already reaches the model as a turn; a
/// handle that is not here is not running, so its edge is met.
#[derive(Debug, Clone, Default)]
pub struct RunningJobs {
    live: BTreeSet<String>,
    /// **Monitors still watching**, by name - what a `monitor` need waits on (`TodoNeed::Monitor`).
    watching: BTreeSet<String>,
    subagents: BTreeSet<String>,
    /// **What each one is about**, by handle: a job's slug and command, a child's prompt. Read only
    /// by [`RunningJobs::likeliest_wait`], so the check's offer names the job a row is LIKE rather
    /// than the first handle in the table.
    about: BTreeMap<String, String>,
    /// **Jobs and children that have ENDED and whose completion the model has not yet digested**,
    /// by handle (or child id), each with how long is left before the backstop releases it. See
    /// [`RunningJobs::digesting`].
    digesting: BTreeMap<String, Duration>,
}

impl RunningJobs {
    /// Nothing running - a reading with no daemon behind it, and the honest one for every board
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

    /// **Whether this handle is running here** - the one read [`unmet_needs`] and the check's
    /// narrowing both take.
    pub fn is_running(&self, handle: &str) -> bool {
        self.live.contains(handle)
    }

    /// **A monitor that is still watching here**, by its name - a builder, like [`Self::running`].
    pub fn watching(mut self, name: impl Into<String>) -> Self {
        self.watching.insert(name.into());
        self
    }

    /// Whether the monitor named `name` is still watching.
    pub fn is_watching(&self, name: &str) -> bool {
        self.watching.contains(name)
    }

    /// A job or child that has ended and whose completion the model has not digested yet, with how
    /// long is left before the backstop lets the check speak anyway. A fact, not a clock: the
    /// caller stops reporting it once the model has had a turn with the completion.
    pub fn digesting(mut self, handle: impl Into<String>, left: Duration) -> Self {
        self.digesting.insert(handle.into(), left);
        self
    }

    /// **The same snapshot with no ending graced** - for a reader that is a DELIVERY rather than a
    /// nag: a row's `when: job` firing is the news itself, like the completion it rides in with,
    /// and must not wait for the model to have digested it.
    pub fn without_grace(mut self) -> Self {
        self.digesting.clear();
        self
    }

    /// **Whether this ended job or child is one the model has not digested yet** - the read
    /// [`unmet_needs`] and [`waits_on_an_undigested_ending`] take.
    pub fn is_digesting(&self, handle: &str) -> bool {
        self.digesting.contains_key(handle)
    }

    /// **A subagent THIS session spawned that is still running**, by its full session id - what
    /// `wait_on` could name as a `child`. See the type's doc for why it is here.
    pub fn subagent(mut self, id: impl Into<String>) -> Self {
        self.subagents.insert(id.into());
        self
    }

    /// What a running job or child is about - a job's slug and command, a child's prompt - for
    /// [`RunningJobs::likeliest_wait`]. A handle with no description still counts as running; it
    /// just cannot be matched.
    pub fn about(mut self, handle: impl Into<String>, text: impl Into<String>) -> Self {
        self.about.insert(handle.into(), text.into());
        self
    }

    /// The running jobs and children, the one most like `row` first: the check's offer of
    /// `wait_on`. Each entry is `(field, handle, about, score)`, the score a cosine over terms
    /// weighed by a smoothed idf; ties keep jobs before children and handle order.
    pub fn likeliest_wait(&self, row: &str) -> Vec<(&'static str, &str, &str, f64)> {
        let candidates: Vec<(&'static str, &str)> = self
            .live
            .iter()
            .map(|h| ("job", h.as_str()))
            .chain(self.subagents.iter().map(|c| ("child", c.as_str())))
            .collect();
        let text = |h: &str| self.about.get(h).map(String::as_str).unwrap_or("");
        let docs: Vec<Vec<String>> = std::iter::once(crate::similarity::terms(row))
            .chain(
                candidates
                    .iter()
                    .map(|(_, h)| crate::similarity::terms(text(h))),
            )
            .collect();
        let mut df: BTreeMap<&str, f64> = BTreeMap::new();
        for d in &docs {
            for t in d.iter().map(String::as_str).collect::<BTreeSet<_>>() {
                *df.entry(t).or_insert(0.0) += 1.0;
            }
        }
        let n = docs.len() as f64;
        let vec = |d: &[String]| -> BTreeMap<String, f64> {
            let mut v: BTreeMap<String, f64> = BTreeMap::new();
            for t in d {
                *v.entry(t.clone()).or_insert(0.0) += 1.0;
            }
            for (t, w) in v.iter_mut() {
                *w *= (1.0 + (n + 1.0) / df.get(t.as_str()).copied().unwrap_or(1.0)).ln();
            }
            v
        };
        let cos = |a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>| -> f64 {
            let dot: f64 = a.iter().filter_map(|(t, x)| b.get(t).map(|y| x * y)).sum();
            let na = a.values().map(|x| x * x).sum::<f64>().sqrt();
            let nb = b.values().map(|x| x * x).sum::<f64>().sqrt();
            if na == 0.0 || nb == 0.0 {
                0.0
            } else {
                dot / (na * nb)
            }
        };
        let q = vec(&docs[0]);
        let mut out: Vec<(&'static str, &str, &str, f64)> = candidates
            .iter()
            .zip(&docs[1..])
            .map(|((kind, h), d)| (*kind, *h, text(h), cos(&q, &vec(d))))
            .collect();
        out.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    pub fn something_to_wait_on(&self) -> Option<(&'static str, &str)> {
        self.live
            .iter()
            .next()
            .map(|h| ("job", h.as_str()))
            .or_else(|| {
                self.subagents
                    .iter()
                    .next()
                    .map(|id| ("child", id.as_str()))
            })
    }
}

/// The sentence a job's or subagent's start result ends with: attach the wait now, while the
/// handle is in hand. Said only when it can apply - `todo_write` seated and a row open.
pub fn attach_now(ctx: &InvokeCtx<'_>, field: &str, handle: &str) -> String {
    if !ctx.plan_open {
        return String::new();
    }
    format!(
        " If a todo row waits on this: `todo_write` {{\"wait_on\": [{{\"content\": \"<the row, \
         quoted exactly>\", \"{field}\": \"{handle}\"}}]}}."
    )
}

/// **How alike a row and a running job must be for the check to say the row "reads like" it.**
/// Below it the offer still names the likeliest one, without claiming they are related.
const LIKE_FLOOR: f64 = 0.15;

/// A job's or child's description cut to one clause for the check's line - the line is folded by
/// the head as one, so it cannot grow a paragraph per running job.
pub fn said_short(about: &str) -> String {
    let one: String = about.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.chars().count() {
        0 => "no description".into(),
        n if n <= 80 => one,
        _ => format!("{}…", one.chars().take(79).collect::<String>()),
    }
}

/// Whether a row waits on a child still inside its room to breathe. Such a row is one the check
/// may not speak about at all - not asked about, and not listed as blocked - because the child is
/// out of the plan's hands. Any need of that shape makes the row quiet.
pub fn waits_on_a_child_with_room_to_breathe(row: &TodoItem, children: &ChildSessions) -> bool {
    row.needs.iter().any(|need| match need {
        TodoNeed::Child { id } => children
            .child(id)
            .is_some_and(|child| child.age < CHILD_ROOM_TO_BREATHE),
        _ => false,
    })
}

/// Whether a row waits on a job that is still running. A job is silent for as long as it runs,
/// where a child is silent only while it is young: the daemon watches every job and submits its
/// settlement as a turn, so there is nothing to steer and no deadline of its own is needed.
pub fn waits_on_a_running_job(row: &TodoItem, jobs: &RunningJobs) -> bool {
    row.needs.iter().any(|need| match need {
        TodoNeed::Job { handle } => jobs.is_running(handle),
        // A monitor still watching is the same silence: its firing wakes the session itself.
        TodoNeed::Monitor { name } => jobs.is_watching(name),
        _ => false,
    })
}

/// Whether a row waits on a room item the room is still working on. The fact is the row's own -
/// the daemon's room pass wrote it - so there is no clock and no snapshot to hand in.
pub fn waits_on_the_room(row: &TodoItem) -> bool {
    row.needs
        .iter()
        .any(|n| matches!(n, TodoNeed::Room { working: true, .. }))
}

/// **The room items a row is the pass's own record of** - every item number its room edges name,
/// loud or quiet. `TodoBoard::set_room_rows` retires by this, which is what lets a board rebuilt
/// from a store still find the rows the pass wrote: the edge is on the row, so no memory is needed.
pub fn room_items(row: &TodoItem) -> Vec<u64> {
    row.needs
        .iter()
        .filter_map(|n| match n {
            TodoNeed::Room { item, .. } => Some(*item),
            _ => None,
        })
        .collect()
}

/// Whether a row waits on a job or child that has ended but whose completion the model has not
/// digested yet: the grace after a wait, so the check does not name a row whose answer is still
/// on its way in. What it guards is the fact, not a timer.
pub fn waits_on_an_undigested_ending(row: &TodoItem, jobs: &RunningJobs) -> bool {
    row.needs.iter().any(|need| match need {
        TodoNeed::Job { handle } => !jobs.is_running(handle) && jobs.is_digesting(handle),
        TodoNeed::Child { id } => jobs.is_digesting(id),
        _ => false,
    })
}

/// When the grace after an ending runs out on its own: the shortest backstop left among the
/// endings these rows wait on, or `None` when no row is in that state. Without it a model that
/// read the completion and went idle would leave the row quiet until somebody acted.
pub fn when_an_ending_is_digested(rows: &[TodoItem], jobs: &RunningJobs) -> Option<Duration> {
    rows.iter()
        .flat_map(|row| row.needs.iter())
        .filter_map(|need| match need {
            TodoNeed::Job { handle } if !jobs.is_running(handle) => {
                jobs.digesting.get(handle.as_str()).copied()
            }
            TodoNeed::Child { id } => jobs.digesting.get(id.as_str()).copied(),
            _ => None,
        })
        .filter(|left| !left.is_zero())
        .min()
}

/// Whether a merge queue row has its review in flight, and so is not the check's to ask about:
/// it waits in the queue for a reviewer, or its reviewer is running and has not been quiet past
/// [`REVIEWER_QUIET_CAP`]. Neither is the gatekeeper's to move.
pub fn queue_review_in_flight(row: &TodoItem, children: &ChildSessions) -> bool {
    if row.by != TodoBy::Queue {
        return false;
    }
    let mut reviewers = row.needs.iter().filter_map(|need| match need {
        TodoNeed::Child { id } => Some(id),
        _ => None,
    });
    match reviewers.next() {
        None => row.status == TodoStatus::Pending,
        Some(id) => children
            .child(id)
            .is_some_and(|child| child.quiet_for.unwrap_or(child.age) < REVIEWER_QUIET_CAP),
    }
}

/// **How long a running reviewer may be quiet before its queue row is asked about** - the same
/// thirty minutes a child gets to breathe ([`CHILD_ROOM_TO_BREATHE`]), measured from its last event
/// rather than its start: a review is long by nature, and only silence says it may be stuck.
pub const REVIEWER_QUIET_CAP: Duration = CHILD_ROOM_TO_BREATHE;

/// **When a queue row quiet only because its reviewer is working becomes askable on its own**  -
/// the clock half of [`queue_review_in_flight`], so a reviewer that wedges is asked about without
/// waiting for somebody to do something. `None` when no row is in that state.
pub fn when_a_queue_reviewer_goes_quiet(
    rows: &[TodoItem],
    children: &ChildSessions,
) -> Option<Duration> {
    rows.iter()
        .filter(|row| row.by == TodoBy::Queue)
        .flat_map(|row| row.needs.iter())
        .filter_map(|need| match need {
            TodoNeed::Child { id } => children.child(id).and_then(|child| {
                REVIEWER_QUIET_CAP
                    .checked_sub(child.quiet_for.unwrap_or(child.age))
                    .filter(|left| !left.is_zero())
            }),
            _ => None,
        })
        .min()
}

/// **Whose row this is, in the words every sentence about a row uses** - `yours`, `the
/// operator's`, the parent's own author string, or `the merge queue's`. One spelling for the
/// check's question and for a completion naming the rows that wait on it.
pub fn whose_row(by: &TodoBy) -> &str {
    match by {
        TodoBy::Operator => "the operator's",
        // The author string itself (`Parent <session id>`), verbatim: a child reading its own
        // board must be able to tell what it decided from what it was told, and by whom.
        TodoBy::Parent(author) => author.as_str(),
        // **The queue is named as the queue**, because that is what it is: not a session, not the
        // person, and not the model's own plan.
        TodoBy::Queue => "the merge queue's",
        TodoBy::Model => "yours",
    }
}

/// When a plan that is quiet only because a child is young becomes speakable: the shortest room
/// to breathe left among the children its rows wait on. This is the clock half of the silence -
/// without it a row would stay quiet until somebody happened to do something.
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

/// A gap of time in words, rounded down to the coarsest unit that is not zero: the reader is
/// judging whether a child has been quiet too long, and a rounded-up minute would be the same
/// overstatement the sentence exists to avoid.
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

/// The plan a turn ended without finishing, as a message for the model, or `None` when there is
/// nothing to say. The trigger is pending work, so the list becomes a contract at the one moment
/// it can be honoured; an empty or fully completed list is silent.
pub fn unfinished_plan(
    rows: &[TodoItem],
    board: &[TodoItem],
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> Option<String> {
    unfinished_plan_for(rows, board, None, children, jobs).map(|(text, _)| text)
}

/// The check's sentence and the row it named, so the caller can remember the choice: the row is
/// named again until it stops being askable, with the operator's ready rows ahead of it. `rows`
/// is the narrowed set, `board` the whole board.
pub fn unfinished_plan_for(
    rows: &[TodoItem],
    board: &[TodoItem],
    chosen: Option<&str>,
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> Option<(String, String)> {
    let PlanAsGraph { ready, blocked, .. } = as_a_graph(rows, board, children, jobs);
    let Some(next) = the_row_to_name(&ready, chosen) else {
        // The degenerate case, said out loud: an unfinished plan whose ready set is empty is a
        // plan to unblock, and silence would read as nothing to do. Except when everything left
        // waits on the operator - then nothing is the model's to move and the check stays quiet.
        if all_on_the_operator(&blocked) {
            return None;
        }
        return (!blocked.is_empty()).then(|| (nothing_can_start(&blocked), String::new()));
    };
    // The count of open rows is not in the text: the idle clock compares that text, so a row
    // added or closed elsewhere would make the same question a new notice and reset the backoff.
    let who = whose_row(&next.by);
    // R93.B4: a row whose own words are a running job's or child's description is work already in
    // flight. Asking about it produces "still running" and offering the wait is the same sentence
    // again, so the check is silent until the thing it names has ended.
    if matches!(next.by, TodoBy::Model | TodoBy::Operator)
        && jobs
            .likeliest_wait(&next.content)
            .first()
            .is_some_and(|(_, _, _, score)| *score >= LIKE_FLOOR)
    {
        return None;
    }
    // The first check that names an operator's row after they add it says who put it there; a row
    // the model has already moved to in progress is one it knows about.
    let operator_added = next.by == TodoBy::Operator
        && next.status == TodoStatus::Pending
        && chosen != Some(next.content.as_str());
    // R93.B2: the state is said, not argued. *"marked in progress and still open"* and *"you were
    // asked about this one already"* invited a restatement in reply (113× *"still in progress"*
    // in one session), which is not a tool call and moves nothing.
    let state = match next.status {
        TodoStatus::InProgress => format!(" — {who} (in progress)"),
        _ => format!(" — {who}"),
    };
    // The verbs differ by author: the model may mark its own row done and may not do that to the
    // operator's, and a parent's row no field of this tool disposes of.
    let advice = match &next.by {
        TodoBy::Operator => {
            "The operator asked for it: do it, or mark it done with `todo_write`'s `operator` \
             field (quote it exactly)."
        }
        TodoBy::Parent(_) => {
            "Your parent asked for it: do it and say so in your reply; they retire the row."
        }
        // The queue's rows are the one kind whose disposal is not a tool call: the verdict takes
        // the row off, so the advice names the two acts that are left.
        TodoBy::Queue => {
            "The merge queue's row; its reviewer has finished or gone quiet: read its reply, or \
             `task_message` it. The verdict takes the row off."
        }
        TodoBy::Model => {
            // Dropping a row by leaving it out went with the old contract: a row left out stays on
            // the board, so the model retires one by marking it done. The phrase below is matched
            // by the head's notice fold, so it is kept whole.
            "Do it, or mark it done with `todo_write`'s `update` field (quote it exactly)."
        }
    };
    // The door for a row that is only waiting, when there is something it could wait on: the
    // model will not reach for a door nobody named. Appended to the advice's own line, because
    // the head folds a notice only while every line after the bullet is the advice it knows.
    let offer = match (&next.by, jobs.likeliest_wait(&next.content).as_slice()) {
        (TodoBy::Model | TodoBy::Operator, [(kind, handle, _, _), rest @ ..]) => {
            let others: Vec<String> = rest
                .iter()
                .take(2)
                .map(|(_, h, a, _)| format!("`{h}` ({})", said_short(a)))
                .collect();
            let others = match others.is_empty() {
                true => String::new(),
                false => format!(" Also running: {}.", others.join(", ")),
            };
            format!(
                " If this row is only waiting on a running job or subagent, attach the wait \
                 instead of answering — `todo_write` {{\"wait_on\": [{{\"content\": \"<the text \
                 above>\", \"{kind}\": \"{handle}\"}}]}} — and this check stays quiet until it \
                 ends.{others}"
            )
        }
        _ => String::new(),
    };
    // **AND WHAT CANNOT START IS SAID, not merely skipped.** A blocked row is invisible to the ask
    // - that is the whole point - so the model would otherwise learn nothing about the rows it
    // cannot be asked for, and a plan whose edges have gone stale would look like a shorter plan.
    let waiting = if blocked.is_empty() {
        String::new()
    } else {
        format!("\n{}", blocked_said(&blocked))
    };
    // The head line says the operator added it; the bullet, the advice and the waiting line keep
    // their shape, because the head's notice fold reads them.
    let head = if operator_added {
        "[todo check] The operator added this, and it is not done".to_string()
    } else {
        "[todo check] Not done".to_string()
    };
    Some((
        format!(
            "{head}:\n  - {}{state}\n{advice}{offer}{waiting}",
            next.content.trim(),
        ),
        next.content.clone(),
    ))
}

/// Is everything that is blocked waiting, in the end, on the operator? A row is, when every
/// unmet need is an unanswered question or a row that is itself on the operator. Empty is
/// `false`: there is nothing to be silent about.
fn all_on_the_operator(blocked: &[(&TodoItem, Vec<(String, NeedFate)>)]) -> bool {
    if blocked.is_empty() {
        return false;
    }
    // A fixed point over the blocked set: start from the rows waiting directly on the operator and
    // add any row whose every unmet need is the operator or a row already known to be - so a cycle
    // or a broken edge is never counted, and the loop ends when a pass adds nothing.
    let mut on: BTreeSet<&str> = BTreeSet::new();
    loop {
        let before = on.len();
        for (row, unmet) in blocked {
            if on.contains(row.content.trim()) {
                continue;
            }
            // R93.B5: a running job and a child still young are roots
            // the check is silent about everywhere else (`todo_write` promises it), so a plan
            // blocked only on them is not a plan to fix either.
            let rooted = unmet.iter().all(|(name, fate)| match fate {
                NeedFate::OperatorAsked | NeedFate::JobRunning | NeedFate::MonitorWatching => true,
                // A room item the room is working on is a root too: not the plan's to move, and a
                // plan blocked only on it is not a plan to fix.
                NeedFate::RoomWorking => true,
                NeedFate::ChildRunning(child) => child.age < CHILD_ROOM_TO_BREATHE,
                NeedFate::NotDone => on.contains(name.trim()),
                _ => false,
            });
            if rooted {
                on.insert(row.content.trim());
            }
        }
        if on.len() == before {
            break;
        }
    }
    blocked
        .iter()
        .all(|(row, _)| on.contains(row.content.trim()))
}

/// Which ready row the check names: the operator's open, ready rows first, then the row already
/// asked about while it is still ready, then the head of the ready set. One function, so the
/// clock, the text and a child's own board cannot rank differently.
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

/// Why a row cannot start: one need the board does not meet, and the reason. Everything that is
/// not a done row is unmet - a condition nobody can evaluate must never read as met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedFate {
    /// The row it names is on the board and is not `Completed`.
    NotDone,
    /// The row it names is one the operator struck off. Not `NotDone`: telling the reader to go and
    /// finish work nobody owes is the nag this check exists not to produce.
    StruckOff,
    /// The name matches no row on the board - a re-worded row, a deleted one, or a name that was
    /// never right. The common case, and the one the message has to make repairable.
    NoSuchRow,
    /// The name matches more than one row, so which one is meant is not knowable - the house rule
    /// [`TodoBoard::set_operator_states`] already keeps for an ambiguous name.
    Ambiguous,
    /// A [`TodoNeed`] kind this build cannot evaluate - see that enum's `Unknown` variant.
    UnknownKind,
    /// A child this daemon is running: the row cannot start until it finishes. It carries the
    /// child's facts and not a verdict - [`child_said`] makes the verdict - so a young child can
    /// carry this fate and never be spoken about.
    ChildRunning(RunningChild),
    /// A background job this session is running: the row cannot start until it ends, any ending.
    /// It carries no payload because a job needs no steering - the daemon delivers its settlement
    /// as a turn of its own.
    JobRunning,
    /// **A monitor still watching** - [`NeedFate::JobRunning`]'s sibling: the row cannot start
    /// until it ends, and the check is silent about the row while it watches.
    MonitorWatching,
    /// A job or child that has ended and whose completion the model has not digested yet. Not met:
    /// the row's answer is in a completion the model has not acted on.
    EndedUnread,
    /// The room is working on the item this row holds, so the row is silent about it: the holder
    /// said what it waits for and the room is carrying it, so there is nothing the plan could do.
    RoomWorking,
    /// The operator was asked a question and has not answered it; the name beside it is the
    /// question. Blocked, and the rest of the plan keeps going.
    OperatorAsked,
}

/// **The rows the check may speak about, read as a graph** - split by whether they can start.
struct PlanAsGraph<'a> {
    /// The READY rows, in [`open_priority`]'s order.
    ready: Vec<&'a TodoItem>,
    /// The rest, each with what it waits for that the board does not give it.
    blocked: Vec<(&'a TodoItem, Vec<(String, NeedFate)>)>,
}

/// The plan as a graph: which of the check's rows can start, and what the rest wait on. `rows`
/// is the narrowed set and `board` the whole board, because an edge is met by a row that is done
/// and the narrowing removes done rows.
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
    PlanAsGraph { ready, blocked }
}

/// What this row waits for that the board does not give it - empty when the row is ready. Names
/// are matched against trimmed content, the same key every other door uses. A child is answered
/// by `children` and a job by `jobs`, and a handle neither holds is an ending: met.
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
                    // A row the operator struck off is not work that is coming, and it is not still
                    // open either: its own fate, whose sentence says to re-word the edge.
                    [one] if one.status == TodoStatus::Cancelled => {
                        out.push((content.clone(), NeedFate::StruckOff))
                    }
                    [_] => out.push((content.clone(), NeedFate::NotDone)),
                    [] => out.push((content.clone(), NeedFate::NoSuchRow)),
                    _ => out.push((content.clone(), NeedFate::Ambiguous)),
                }
            }
            // A child the daemon is running is not met - it has not finished - and what is pushed
            // is the daemon's reading, not a verdict. The id is the name the message prints, and
            // what `task_result` collects by.
            TodoNeed::Child { id } => {
                if let Some(child) = children.child(id) {
                    out.push((id.clone(), NeedFate::ChildRunning(*child)));
                } else if jobs.is_digesting(id) {
                    // **Finished, and its completion not digested yet** - see
                    // [`waits_on_an_undigested_ending`].
                    out.push((id.clone(), NeedFate::EndedUnread));
                }
            }
            // A job the session is running is not met, and its handle is the name the message
            // prints because that is what `job_output` is given; a handle the snapshot does not
            // hold is a job that ended. A monitor still watching is the same, one name over.
            TodoNeed::Monitor { name } => {
                if jobs.is_watching(name) {
                    out.push((name.clone(), NeedFate::MonitorWatching));
                }
            }
            TodoNeed::Job { handle } => {
                if jobs.is_running(handle) {
                    out.push((handle.clone(), NeedFate::JobRunning));
                } else if jobs.is_digesting(handle) {
                    // **Ended, and its completion not digested yet** - not met until the model has
                    // had a turn with it: see [`waits_on_an_undigested_ending`].
                    out.push((handle.clone(), NeedFate::EndedUnread));
                }
            }
            // The room is working on it, so the edge is not met; the name printed is the room's own
            // `#3`, which is what its verbs take. A quiet room edge is met: nothing outside will
            // move the item, so it is the holder's own work.
            TodoNeed::Room {
                item,
                working: true,
            } => out.push((format!("#{item}"), NeedFate::RoomWorking)),
            TodoNeed::Room { working: false, .. } => {}
            // The operator has not answered, so the edge is not met, and the name printed is the
            // question - what the model has to work out while it waits. An answered edge is met.
            TodoNeed::Operator { question, .. } => {
                out.push((question.clone(), NeedFate::OperatorAsked))
            }
            TodoNeed::Answered { .. } => {}
            TodoNeed::Unknown => out.push((String::new(), NeedFate::UnknownKind)),
        }
    }
    out
}

/// **One blocked row, as the message says it** - `` `deploy` waits on `run the tests` (still open) ``.
fn blocked_line(row: &TodoItem, unmet: &[(String, NeedFate)]) -> String {
    let waits: Vec<String> = unmet
        .iter()
        .map(|(name, fate)| match fate {
            NeedFate::NotDone => format!("`{name}` (still open)"),
            // **Said as the OPERATOR's act and not as a plan state**, because that is what the
            // reader has to act on: nobody is going to finish this row, and the fix is the model's
            // own - drop the need, or point it at work that can happen.
            NeedFate::StruckOff => format!(
                "`{name}` (struck off by the operator — this edge can never be met; re-send this \
                 row in `todos` with that need removed)"
            ),
            NeedFate::NoSuchRow => format!("`{name}` (no such row on this board)"),
            NeedFate::Ambiguous => format!("`{name}` (two rows on this board say that)"),
            NeedFate::UnknownKind => {
                "a dependency of a kind this build cannot evaluate".to_string()
            }
            // A child is not named the way a row is: its id is handed to `task_result`, so the
            // sentence says which child and how long it has run before anything else.
            NeedFate::ChildRunning(child) => format!(
                "the child `{name}`, running {} — {}",
                human_gap(child.age),
                child_said(child)
            ),
            // **A job is named by its handle**, the one `job_list` and `job_output` are given, and
            // the sentence says the two things the reader needs and nothing else: the blocker is
            // still there, and it is not theirs to move - the settlement is already on its way.
            NeedFate::JobRunning => format!(
                "the job `{name}`, still running — its completion reaches you as soon as it ends"
            ),
            NeedFate::MonitorWatching => format!(
                "the monitor `{name}`, still watching — its firing reaches you as soon as it fires"
            ),
            // **A room item is named by the number the room's own verbs take**, and the sentence
            // says the one thing that ends the silence: the wait, which the holder clears itself.
            NeedFate::RoomWorking => format!(
                "the room item `{name}`, which you hold and told the room you are waiting on — the \
                 check stays quiet until that wait ends"
            ),
            NeedFate::EndedUnread => format!(
                "`{name}`, which has ended — its completion is reaching you; read it before \
                 this row is asked about"
            ),
            // The operator's sentence carries the instruction, because it is the one blocker the
            // model may resolve without the blocker moving: keep going, and take the wait off with
            // the row's own words if the answer is worked out.
            NeedFate::OperatorAsked => format!(
                "the OPERATOR — they were asked \"{name}\" and have not answered yet. Keep going \
                 with the rest of the plan meanwhile, and try to work the question out yourself: if \
                 you find the answer, take the wait off — `todo_write` {{\"wait_on\": \
                 [{{\"content\": \"{}\", \"detach\": true}}]}} — and say in your reply what you \
                 decided and why",
                row.content.trim()
            ),
        })
        .collect();
    format!("`{}` waits on {}", row.content.trim(), waits.join(", "))
}

/// What a live child is doing, as the daemon can see it: working, between turns, quiet, or
/// showing nothing yet. One judgement, so the idle check's sentence and `task_result` cannot
/// disagree about one child - between turns is a word away, quiet is a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildDoing {
    /// A turn is in flight and the child's own log moved inside the window.
    Working,
    /// **No turn is in flight and the child is still here** - parked, and reachable by a message.
    BetweenTurns,
    /// A turn is in flight and nothing has been published for longer than the window.
    Quiet,
    /// Nothing has been published in it at all, so the daemon has no evidence either way.
    NoEvidence,
}

impl ChildDoing {
    /// **The state in the words both doors use** - one spelling, so a reader who has read the
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

/// Which of those states this reading is: the two axes are whether anything is running and
/// whether anything has moved. A reader that wants the evidence rather than the name takes
/// [`child_evidence`].
pub fn child_doing(child: &RunningChild) -> ChildDoing {
    match (child.working, child.quiet_for) {
        // Moving: this is the state the check must NOT speak about.
        (true, Some(gap)) if gap < CHILD_QUIET_ENOUGH_TO_LOOK => ChildDoing::Working,
        // A turn is in flight and nothing has come out of it for longer than the window.
        (true, _) => ChildDoing::Quiet,
        // No turn in flight and something has been published: the turn ended and the child is
        // parked. A child still opening reads the same way; what this can say is that nothing is
        // running in it.
        (false, Some(_)) => ChildDoing::BetweenTurns,
        (false, None) => ChildDoing::NoEvidence,
    }
}

/// The evidence under the name, in the one clause both doors print - a second rendering would be
/// a second answer about one child.
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

/// What the check says about a child it is waiting on: the evidence and the steering, in one
/// clause. Liveness alone says the child has not died and nothing about whether it is stuck, so
/// the steering follows [`ChildDoing`].
fn child_said(child: &RunningChild) -> String {
    match child_doing(child) {
        ChildDoing::Working => format!("it is working ({})", child_evidence(child)),
        // The state that had no sentence: a child whose turn has ended is parked, not stuck, and
        // the one door that reaches a parked child is its parent's message.
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

/// What the check says about the rows it cannot ask for, in one line, so the model learns the
/// plan is not empty but stuck. Three leads, because a row, a child and a job ask different things
/// of the reader; a row with more than one kind of need goes with the furthest one.
fn blocked_said(blocked: &[(&TodoItem, Vec<(String, NeedFate)>)]) -> String {
    let mut said: Vec<String> = Vec::new();
    for lead in [
        "cannot start yet",
        "waiting on a child that is still running",
        "waiting on a job that is still running",
        "waiting on the operator",
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
/// filter there. A row goes with the operator, then the child, then the job, then plain rows: the
/// lead names the thing the plan is least able to answer.
fn lead_of(unmet: &[(String, NeedFate)]) -> &'static str {
    // **The operator first**: a person is the blocker the plan can least move, and the lead that
    // carries the instruction to keep going and try the question meanwhile.
    if unmet
        .iter()
        .any(|(_, fate)| matches!(fate, NeedFate::OperatorAsked))
    {
        "waiting on the operator"
    } else if unmet
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

/// `1 row` / `2 rows` - the count the leads above open with.
fn rows_count(n: usize) -> String {
    match n {
        1 => "1 row".to_string(),
        n => format!("{n} rows"),
    }
}

/// The sentence for an unfinished plan with an empty ready set: every row left waits on another,
/// so there is nothing to work on and something to unblock. Saying nothing would read as nothing
/// to do; each extra clause is added only when such a row is here.
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
             look at: its completion reaches you as soon as it ends, and the row can start then",
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
// length of list to truncate. The operator's ruling - *"only one todo at a time, so a model will not
// be defocused"* - removes the thing that constant existed for.

/// The daemon-side half of `todo_write`'s `target`: reaching a child session's board. Built per
/// session with the caller already bound, so it decides everything the safety turns on - that the
/// target is the caller's own child, and the author, which never comes from the call.
pub trait ChildTodos: Send + Sync {
    /// Add or state-update ROWS on the named child's board, authored as the CALLER this
    /// implementation was built for. `Err` is the refusal, written to be shown to the model
    /// as it stands.
    fn upsert_child(
        &self,
        target: &str,
        rows: &[(String, TodoStatus, Vec<TodoNeed>)],
    ) -> Result<Vec<TodoItem>, String>;

    /// Whether `asked` names a child of the CALLER, and its full session id if so: a wait on a
    /// session that is not this one's child is a wait nothing here will ever end. Defaulted to a
    /// refusal, so a resolver that does not know its children says so by name.
    fn own_child(&self, asked: &str) -> Result<String, String> {
        Err(format!(
            "`{asked}` cannot be checked against this session's children — this runtime does not \
             know them"
        ))
    }
}

/// The write tool. Holds the board; the harness holds the same `Arc`.
pub struct TodoWriteTool {
    board: Arc<TodoBoard>,
    /// The daemon's child-board resolver - `None` in a runtime with no daemon behind it, in which
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

    /// Seat the tool with a way to reach CHILDREN's boards - the daemon's half of `target`. The
    /// board stays the session's own; nothing here can write it as anybody else.
    pub fn with_children(board: Arc<TodoBoard>, children: Arc<dyn ChildTodos>) -> Self {
        TodoWriteTool {
            board,
            children: Some(children),
        }
    }
}

impl Tool for TodoWriteTool {
    fn open_plan(&self) -> Option<bool> {
        Some(
            self.board
                .snapshot()
                .iter()
                .any(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress)),
        )
    }

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
             your reply.\n\n`update` moves ONE OR MORE of YOUR OWN rows — the way to mark \
             rows without re-sending the list. It takes `content` (the row's \
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
               reaches you as soon as it ends, so there is nothing to watch and nothing to look at \
               — and the row is asked about once it is over. Use it for work that has to land \
               first: a row that pushes a branch waits on the job that tests it. This is an EDGE \
               and not a clock: a row that FIRES when a job ends is the operator's `when`, which \
               is not yours to write.\n\n`wait_on` marks a row ALREADY on the board — yours OR \
               the operator's — as waiting on a running job or a subagent, without restating it: \
               {\"wait_on\": [{\"content\": \"<the row, quoted exactly>\", \"job\": \"j123\"}]} \
               (the handle or the name `job_list` prints), or \"child\": \"<the session id `task` \
               handed back>\" in place of `job`. Use it when the check names a row you can only \
               answer with *the job is still running*: while that job or subagent runs the check \
               is SILENT about the row, and once it ends the row is asked about again. It changes \
               no row's words or status — on the operator's row it only says *not now*. \
               {\"content\": \"…\", \"detach\": true} takes the row's job and subagent waits \
               off again. The job must be one of this session's and still running, the child one \
               you spawned; anything else is refused by name. `wait_on` goes in a call of its own.\n\nA \
               row may also WAIT ON THE OPERATOR — a decision only they can make. Give them a \
               CHOICE: `needs` takes {\"kind\": \"operator\", \"question\": \"<one question>\", \
               \"options\": [\"<choice>\", \"<choice>\"]} with at least TWO options, or `wait_on` \
               takes {\"content\": \"<the row>\", \"operator\": {\"question\": …, \"options\": \
               […]}} for a row already on the board. Their screen then says LOUDLY that you are \
               waiting on them and shows the question as a card: they pick an option (with a note \
               if they like), type their own answer, or abstain. That row is NOT asked about while \
               it waits, and THE REST OF THE PLAN KEEPS GOING — the check keeps asking about every \
               other row that can start, so carry on with them. Meanwhile try to work the question \
               out yourself; if you can, take the wait off with {\"content\": \"…\", \"detach\": \
               true} and say what you decided. Their answer reaches you at once as an `[answer]` \
               line, the row is unblocked, and the reply stays on the row. An abstention is an \
               answer too and unblocks the row: they will not decide, so proceed and state the \
               assumption you proceed on.",
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
                                    // R93.B10: the tagged forms below are objects, and a
                                    // schema of strings only is one a constrained model cannot
                                    // emit them under.
                                    "items": {"anyOf": [
                                        {"type": "string"},
                                        {"type": "object", "properties": {
                                            "kind": {"type": "string", "enum": ["row", "child", "job", "monitor", "operator"]},
                                            "name": {"type": "string"},
                                            "content": {"type": "string"},
                                            "id": {"type": "string"},
                                            "handle": {"type": "string"},
                                            "question": {"type": "string"},
                                            "options": {"type": "array", "items": {"type": "string"}}
                                        }, "required": ["kind"]}
                                    ]},
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
                                                     job runs. {\"kind\": \"monitor\", \
                                                     \"name\": \"…\"} names a MONITOR you armed \
                                                     with `monitor` and waits for it to fire or \
                                                     end, silently while it watches — a one-shot \
                                                     timer monitor is how a row says come back \
                                                     to this later. {\"kind\": \"operator\", \
                                                     \"question\": \"…\", \"options\": [\"…\", \
                                                     \"…\"]} waits on the OPERATOR's answer to a \
                                                     question (at least two options; they can \
                                                     also type their own answer or abstain) — the \
                                                     rest of the plan keeps going meanwhile. \
                                                     Optional, and empty is the ordinary \
                                                    row: a row that names nothing is ready as soon \
                                                    as it is open. A row name that matches no \
                                                    row, or two, blocks this row and the check says \
                                                    which one it could not place."
                                }
                            },
                            "required": ["content", "status"]
                        }
                    },
                    "wait_on": {
                        "type": "array",
                        "description": "Rows ALREADY on the board — yours or the operator's, \
                                        quoted exactly — that wait on a running job or a \
                                        subagent you spawned, or on the operator's answer to a \
                                        question. The check is silent about such a row until \
                                        the job or subagent ends or the operator answers; the \
                                        rest of the plan keeps being asked about. Each entry has \
                                        `content` and exactly one of `job` (a handle or name \
                                        `job_list` prints), `child` (a session id `task` handed \
                                        back), `operator` ({\"question\": \"…\", \"options\": \
                                        [at least two choices]}) or `detach: true` (take its \
                                        waits off, the operator's question included). Several \
                                        entries quoting one row make it wait on ALL of them — \
                                        two jobs, a job and a child, any mix. Words and status \
                                        are never changed. A call of its own.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "job": {"type": "string"},
                                "child": {"type": "string"},
                                "operator": {
                                    "type": "object",
                                    "properties": {
                                        "question": {"type": "string"},
                                        "options": {
                                            "type": "array",
                                            "items": {"type": "string"},
                                            "minItems": 2
                                        }
                                    },
                                    "required": ["question", "options"]
                                },
                                "detach": {"type": "boolean"}
                            },
                            "required": ["content"]
                        }
                    },
                    "target": {
                        "type": "string",
                        "description": "Write onto a CHILD session's board instead of \
                                        your own: the child's session id, its slug, or the \
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

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        // The session's job table, read once: `wait_on` checks a handle against it and a `job`
        // need in `todos` is resolved through it (R92.14) - the same rule both ways.
        let jobs = ctx.backend.processes().map(|host| host.jobs());
        self.write(args, jobs.as_deref())
    }
}

impl TodoWriteTool {
    /// **The whole of `todo_write`, over a job table in hand** - `invoke` is this with the
    /// session's own table. A parameter rather than a read, as [`Self::wait_on`]'s is, so the
    /// rules that read the table are assertable without a cgroup tree.
    pub(crate) fn write(&self, args: &Value, jobs: Option<&[crate::exec::JobView]>) -> Invocation {
        // An unknown top-level field is refused by name: three of these fields are easy to confuse
        // and `target` aims at another session's board, so a field silently ignored is how a wrong
        // write becomes a quiet one.
        if let Some(obj) = args.as_object() {
            for key in obj.keys() {
                if !matches!(
                    key.as_str(),
                    "todos" | "update" | "operator" | "target" | "wait_on"
                ) {
                    return Invocation::failed(
                        format!("`{key}` is not an argument of `todo_write`"),
                        "**nothing was written.** The arguments are `todos` (the whole list), \
                         `update` (one of YOUR rows, quoted), `operator` (one of the OPERATOR's \
                         rows, quoted), `target` (a child's board) and `wait_on` (rows already on \
                         the board, quoted, that wait on a `job`, a `child`, the `operator`'s \
                         answer, or `detach: true` to take the waits off).",
                    );
                }
            }
        }
        // **`wait_on` is a call of its own.** It attaches an edge to a row ALREADY on the board, so
        // a `todos` beside it could add or restate that very row in the same breath - and which of
        // the two edges then stands would be an accident of the order below. One call, one act.
        if let Some(waits) = args.get("wait_on") {
            for field in ["todos", "update", "operator", "target"] {
                if args.get(field).is_some() {
                    return Invocation::failed(
                        format!("`wait_on` and `{field}` do not mix"),
                        format!(
                            "**nothing was written.** `wait_on` marks rows ALREADY on the board \
                             as waiting; send `{field}` in a call of its own, before or after."
                        ),
                    );
                }
            }
            return self.wait_on(waits, jobs);
        }
        // **A MISSING `todos` IS NOT A MISSING ARGUMENT ANY MORE.** `update` and `operator` name a
        // row on their own, which is the whole point of `update`; what is refused is a call that
        // names no row by ANY of the three - see the block below the target path.
        let mut rows = match args.get("todos").and_then(|v| v.as_array()) {
            Some(list) => match parse_rows(list) {
                Ok(rows) => rows,
                Err(inv) => return inv,
            },
            None => Vec::new(),
        };
        // The target path runs before anything of this session's own moves and never reaches
        // `board` at all: everything below is this board's two-author contract, and none of it may
        // leak across sessions.
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
            // One call, one board: `operator` and `update` move a row here, `target` writes onto a
            // child's, so either beside `target` is a wrong write waiting to happen.
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
                // the author never comes from the wire - see `ChildTodos::upsert_child`'s doc.
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
        // The title cap is here and not in `upsert_model` because this call is all or nothing: a
        // list refused after both halves moved would leave them drifted. `before` is read above the
        // moves; a `job` name and a child's slug are resolved to the ids every reader keys on.
        if let Err(inv) = resolve_child_needs(&mut rows, self.children.as_deref(), jobs) {
            return inv;
        }
        if let Err(inv) = resolve_job_needs(&mut rows, jobs) {
            return inv;
        }
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
        // A call that names no row by any field is refused: an empty `todos` used to mean clear
        // the board, and with no delete it means nothing, so it must not read as something done.
        if rows.is_empty() && updates.is_empty() && ops.is_empty() {
            return Invocation::failed(
                "this call names no rows",
                "**nothing was written**: there is no delete and no way to clear the board. \
                 `todos` is the whole list, `update` moves one of YOUR rows by quoting it, \
                 `operator` moves one of the operator's. Send one of the three — and to retire a \
                 row, mark it `completed` rather than leaving it out.",
            );
        }
        // Every quote resolves before anything moves, on both halves at once, so a name that does
        // not resolve costs nothing at all.
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
        // What this call started with, so the reply can say which rows it did not remove - only
        // when `todos` was sent, since a call that names one row never said what the list is.
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
        // The list back to the model, as it now stands - so the next call is written against what
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

impl TodoWriteTool {
    /// `wait_on`: mark rows already on the board as waiting on a job or a subagent, or take that
    /// off. The handle is checked here against what this session actually has - a wait nothing will
    /// ever end would not even keep the row quiet - and anything else is refused by name.
    fn wait_on(&self, v: &Value, jobs: Option<&[crate::exec::JobView]>) -> Invocation {
        let refused = |what: String, why: String| {
            Invocation::failed(what, format!("**nothing was written.** {why}"))
        };
        let shape = || {
            "`wait_on` is a list of {\"content\": \"<the row, quoted exactly>\", \"job\": \"<a \
             handle or name `job_list` prints>\"}, or \"child\": \"<a session id `task` handed \
             back, or its slug>\" in place of `job`, or \"operator\": {\"question\": \"…\", \"options\": \
             [\"…\", \"…\"]} to wait on the operator's answer, or \"detach\": true to take the \
             row's waits off."
                .to_string()
        };
        let Some(list) = v.as_array() else {
            return refused("`wait_on` needs to be a list".into(), shape());
        };
        if list.is_empty() {
            return refused("`wait_on` is empty".into(), shape());
        }
        let mut plan: Vec<(String, Wait)> = Vec::with_capacity(list.len());
        for (i, entry) in list.iter().enumerate() {
            let n = i + 1;
            let Some(obj) = entry.as_object() else {
                return refused(format!("wait_on entry {n} is not an object"), shape());
            };
            for key in obj.keys() {
                if !matches!(
                    key.as_str(),
                    "content" | "job" | "child" | "detach" | "operator"
                ) {
                    return refused(
                        format!("wait_on entry {n} has an unknown field `{key}`"),
                        shape(),
                    );
                }
            }
            let Some(content) = obj.get("content").and_then(|c| c.as_str()) else {
                return refused(format!("wait_on entry {n} has no `content`"), shape());
            };
            let said: Vec<&str> = ["job", "child", "detach", "operator"]
                .into_iter()
                .filter(|k| obj.contains_key(*k))
                .collect();
            let [which] = said.as_slice() else {
                return refused(
                    format!(
                        "wait_on entry {n} names {} of `job`, `child`, `operator` and `detach`",
                        said.len()
                    ),
                    "each entry says exactly ONE thing about its row: the job it waits on, the \
                     child it waits on, the question it waits on the operator for, or \
                     `\"detach\": true`. Send two entries for two waits."
                        .into(),
                );
            };
            let wait = match *which {
                "detach" => {
                    if obj.get("detach") != Some(&Value::Bool(true)) {
                        return refused(
                            format!("wait_on entry {n} has a `detach` that is not `true`"),
                            shape(),
                        );
                    }
                    Wait::Detach
                }
                // **The operator, asked** - no handle to resolve: the question is the wait, and it
                // ends when they answer it (or when the model takes it off having worked it out).
                "operator" => {
                    let o = obj.get("operator");
                    let question = o
                        .and_then(|o| o.get("question"))
                        .and_then(|q| q.as_str())
                        .unwrap_or("");
                    match operator_need(question, o.and_then(|o| o.get("options"))) {
                        Ok(need) => Wait::On(need),
                        Err(why) => {
                            return refused(
                                format!("wait_on entry {n}'s `operator` is refused: {why}"),
                                OPERATOR_SHAPE.into(),
                            );
                        }
                    }
                }
                "job" => {
                    let Some(asked) = obj.get("job").and_then(|j| j.as_str()) else {
                        return refused(
                            format!("wait_on entry {n}'s `job` is not a string"),
                            shape(),
                        );
                    };
                    let Some(table) = jobs else {
                        return refused(
                            format!("`{asked}` cannot be waited on: this session runs no jobs"),
                            "this backend has no process host, so there is no job here that could \
                             end."
                                .into(),
                        );
                    };
                    let job = match crate::builtins::jobs::resolve_job_in(table, asked) {
                        Ok(job) => job,
                        Err(inv) => return inv,
                    };
                    if !job.state.is_running() {
                        return refused(
                            format!(
                                "`{asked}` has already ended ({}) — there is nothing to wait for",
                                job.state.word()
                            ),
                            format!(
                                "read what it said with `job_output {}` and act on the row now.",
                                job.id.0
                            ),
                        );
                    }
                    // **The full handle goes on the row**, whatever the call spelled: it is what
                    // `job_fate` and `RunningJobs` are asked about, and a name is the agent's word
                    // for a job while it is in the table - not a key that survives a restart.
                    Wait::On(TodoNeed::Job { handle: job.id.0 })
                }
                _ => {
                    let Some(asked) = obj.get("child").and_then(|c| c.as_str()) else {
                        return refused(
                            format!("wait_on entry {n}'s `child` is not a string"),
                            shape(),
                        );
                    };
                    let Some(children) = &self.children else {
                        return refused(
                            format!("`{asked}` cannot be waited on: this session has no children"),
                            "this runtime seats `todo_write` without a child resolver, so no \
                             session here was spawned by this one."
                                .into(),
                        );
                    };
                    match children.own_child(asked) {
                        Ok(id) => Wait::On(TodoNeed::Child { id }),
                        Err(why) => {
                            return refused(
                                format!("`{asked}` cannot be waited on: {why}"),
                                shape(),
                            );
                        }
                    }
                }
            };
            plan.push((content.to_string(), wait));
        }
        let (mine, theirs) = match self.board.set_waits(&plan) {
            Ok(counts) => counts,
            Err(why) => {
                return refused(
                    format!("nothing was marked: {why}"),
                    "quote a row's `content` exactly as the list shows it.".into(),
                );
            }
        };
        let mut out = render(&self.board.snapshot());
        out.push_str(&format!(
            "\nwaits changed on {mine} of your rows and {theirs} of the operator's (the rest \
             already said that). A row waiting on a running job or a subagent is not asked about \
             until it ends; then it is asked about again. A row waiting on the operator is not \
             asked about until they answer — their heads show the question loudly, the answer \
             reaches you the moment they give it, and the rest of the plan keeps going meanwhile. Its words and status are unchanged. \
             Restating one of YOUR rows in `todos` restates its `needs` too, so a `todos` entry \
             without the wait takes it off.\n"
        ));
        Invocation::ok(out)
    }
}

/// The title cap, in one place: the check every write of a board row's words passes. It bounds
/// what a call writes and never what a row is, so a row already on this half is being restated
/// and the cap has nothing to say; the whole list is checked before any of it is written.
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

/// What a title over the cap is told, beside the refusal: the cap, where the number comes from,
/// and where the rest of the row goes. One spelling, because both writers that can write a new
/// row reach it.
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

/// Parse and validate a `todos` list - the shared gate both boards' writes pass. Each row comes
/// back as `(content, status, needs)`, because a plan's order is part of the plan. An unknown
/// field inside an entry is refused by name: ignoring it turns a wrong write into a silent one.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
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

/// Parse a `{content, status}` list - `update` and `operator`, one function. They differ only in
/// which half of the board they aim at, which is the board's business; nothing here can add or
/// remove a row, and an unknown key is refused by name.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
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

/// A `job` need's name, turned into the handle every reader asks about. A name the table holds
/// becomes that job's handle, a name that fits two jobs is refused, and a name it does not hold is
/// kept only when it is handle-shaped - it may be one this session ran before a restart.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn resolve_job_needs(
    rows: &mut [(String, TodoStatus, Vec<TodoNeed>)],
    jobs: Option<&[crate::exec::JobView]>,
) -> Result<(), Invocation> {
    let Some(table) = jobs else {
        return Ok(());
    };
    for (content, _, needs) in rows.iter_mut() {
        for need in needs.iter_mut() {
            let TodoNeed::Job { handle } = need else {
                continue;
            };
            if let Some(j) = table.iter().find(|j| j.id.0 == *handle) {
                *handle = j.id.0.clone();
                continue;
            }
            let named: Vec<&crate::exec::JobView> = table
                .iter()
                .filter(|j| j.slug.as_deref() == Some(handle.as_str()))
                .collect();
            match named.as_slice() {
                [one] => *handle = one.id.0.clone(),
                [] if job_handle_shaped(handle) => {}
                _ => {
                    let inv = match crate::builtins::jobs::resolve_job_in(table, handle) {
                        Err(inv) => inv,
                        Ok(_) => unreachable!("resolved above"),
                    };
                    return Err(Invocation::failed(
                        format!(
                            "nothing was written: the row `{content}` needs a job `{handle}`, \
                             and {}",
                            if named.is_empty() {
                                "no job of this session has that name or handle".to_string()
                            } else {
                                format!("{} jobs have that name", named.len())
                            }
                        ),
                        format!(
                            "a `job` need names a job by the handle `bash` and `job_list` print \
                             (`j…`), or by the name it was started under while it is in this \
                             session's table; the handle is what goes on the row.\n\n{}",
                            inv.payload
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A child named in `needs` by its slug is stored as its session id, through the door `wait_on`'s
/// `child` goes through. A `job` need that names one of this session's children becomes a `child`
/// need; a session id is never touched.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn resolve_child_needs(
    rows: &mut [(String, TodoStatus, Vec<TodoNeed>)],
    children: Option<&dyn ChildTodos>,
    jobs: Option<&[crate::exec::JobView]>,
) -> Result<(), Invocation> {
    // A `job` need that names a job of this session stays a job need, whatever a child is called.
    let a_job = |name: &str| {
        jobs.is_some_and(|t| {
            t.iter()
                .any(|j| j.id.0 == name || j.slug.as_deref() == Some(name))
        })
    };
    let Some(children) = children else {
        return Ok(());
    };
    for (content, _, needs) in rows.iter_mut() {
        for need in needs.iter_mut() {
            match need {
                TodoNeed::Child { id } if !id.starts_with("s-") => match children.own_child(id) {
                    Ok(full) => *id = full,
                    Err(why) => {
                        return Err(Invocation::failed(
                            format!(
                                "nothing was written: the row `{content}` needs a child `{id}`"
                            ),
                            format!(
                                "{why}\n\na `child` need names a subagent you started, by the \
                                 handle `task` handed back or by its slug."
                            ),
                        ));
                    }
                },
                TodoNeed::Job { handle } if !job_handle_shaped(handle) && !a_job(handle) => {
                    if let Ok(full) = children.own_child(handle) {
                        *need = TodoNeed::Child { id: full };
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Whether a name is shaped like a handle this daemon mints: a job's `j<digits>`, or a subagent's
/// `s-…` session id (a subagent is backgrounded through the same sink).
fn job_handle_shaped(name: &str) -> bool {
    name.strip_prefix('j')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        || name.starts_with("s-")
}

/// One `needs` entry: a plain string (the other row's own words), or the tagged form the store and
/// the wire carry. A kind this build cannot evaluate is refused by name, because a need that
/// cannot be answered must never read as met. Liveness is the daemon's fact, not this one's.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
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
    let monitor = |name: &str| -> Result<TodoNeed, Invocation> {
        if name.trim().is_empty() {
            return Err(Invocation::failed(
                format!("{where_} names an empty monitor"),
                "a `monitor` need names a monitor by the `name` it was armed under with `monitor`; \
                 an empty name waits for nothing while saying it waits.",
            ));
        }
        Ok(TodoNeed::Monitor {
            name: name.trim().to_string(),
        })
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
            Some("monitor") => match o.get("name").and_then(|n| n.as_str()) {
                Some(name) => monitor(name),
                None => Err(Invocation::failed(
                    format!("{where_} has kind `monitor` and no `name`"),
                    "a `monitor` need names a monitor by the `name` it was armed under with \
                     `monitor` — the one `job_list` and the monitors pane show.",
                )),
            },
            Some("operator") => {
                let question = o.get("question").and_then(|q| q.as_str()).unwrap_or("");
                operator_need(question, o.get("options")).map_err(|why| {
                    Invocation::failed(format!("{where_} is refused: {why}"), OPERATOR_SHAPE)
                })
            }
            // **An answer is the OPERATOR's word, never the model's.** The kind exists on the board
            // (it is what an answered `operator` edge becomes) and it is refused here by name, so a
            // model cannot write itself an answer and start the row on it.
            Some("answered") => Err(Invocation::failed(
                format!("{where_} has kind `answered`, which only the operator's answer writes"),
                "an `answered` edge is the operator's reply to an `operator` question, put on the \
                 row when they answer it; you cannot write one. A row you restate keeps the answers \
                 it already has, so leave them out.",
            )),
            Some(other) => Err(Invocation::failed(
                format!("{where_} has kind `{other}`, which this build cannot evaluate"),
                "the kinds of need are `row` — another row on this list, by its exact `content` — \
                 `child` — a subagent you started with `task`, by the session id it gave you — and \
                 `job` — a background job, by the handle `job_list` prints — `monitor` — a monitor \
                 you armed, by its name, until it fires or ends — and `operator` — a \
                 question for the operator, with at least two `options`. Send a row's name as a \
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

/// **What an `operator` wait looks like**, for the sentence a refusal ends with.
const OPERATOR_SHAPE: &str = "an `operator` wait is {\"question\": \"<one question for the \
     operator>\", \"options\": [\"<choice>\", \"<choice>\", …]} — as a need on one of your \
     rows, {\"kind\": \"operator\", \"question\": …, \"options\": […]}; through `wait_on`, \
     {\"content\": \"<the row>\", \"operator\": {\"question\": …, \"options\": […]}}. At least \
     two options, each a short distinct choice: the operator is given them as a list to pick from, \
     and can always add a note, type their own answer or abstain besides.";

/// An `operator` wait, checked: the one validation `todos`' `needs` and `wait_on` share. The
/// question must say something and there must be a real choice - at least two options, none empty
/// and no two the same. Free text and an abstention are the card's.
fn operator_need(question: &str, options: Option<&Value>) -> Result<TodoNeed, String> {
    let question = question.trim();
    if question.is_empty() {
        return Err("the question is empty — say what you need the operator to decide".into());
    }
    let Some(list) = options.and_then(|o| o.as_array()) else {
        return Err(
            "it has no `options` list — the operator is given a choice, so offer one".into(),
        );
    };
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for (i, o) in list.iter().enumerate() {
        let Some(text) = o.as_str().map(str::trim) else {
            return Err(format!("option {} is not a string", i + 1));
        };
        if text.is_empty() {
            return Err(format!("option {} is empty", i + 1));
        }
        if out.iter().any(|seen| seen == text) {
            return Err(format!("option {} repeats `{text}`", i + 1));
        }
        out.push(text.to_string());
    }
    if out.len() < 2 {
        return Err(format!(
            "it offers {} option{} — the operator must be given a CHOICE, so offer at least two \
             (they can always type their own answer besides)",
            out.len(),
            if out.len() == 1 { "" } else { "s" }
        ));
    }
    Ok(TodoNeed::Operator {
        question: question.to_string(),
        options: out,
    })
}

/// The board as the model reads it, for a reader outside this tool: the same text a `todo_write`
/// answers with, so a cleared conversation starts in the words, marks and authors the model will
/// meet in its next answer.
pub fn board_text(todos: &[TodoItem]) -> String {
    render(todos)
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
        // And who wrote it, or `operator` is a field the model cannot aim: a row marked with the
        // operator's name moves with `operator`, one marked yours with `update`, and a parent's with
        // no field at all. Content is printed verbatim - it is the name to quote back.
        let author = match &t.by {
            TodoBy::Operator => "the operator's".to_string(),
            // **The operator's own string, verbatim and in full** - `Parent s-…` - because a
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
            // And what it waits for: an edge the model cannot see is an edge it cannot maintain, so
            // the first rewrite of a row's text would silently break every edge into it.
            {
                let named: Vec<String> = t
                    .needs
                    .iter()
                    // A room edge with nothing coming is not a wait, and the line says `waiting on`:
                    // listing it would tell the model to wait for what it must finish.
                    .filter(|need| {
                        !matches!(
                            need,
                            TodoNeed::Answered { .. } | TodoNeed::Room { working: false, .. }
                        )
                    })
                    .map(|need| match need {
                        TodoNeed::Row { content } => format!("`{}`", content.trim()),
                        // **The child, by its id**, because that is what the model has to hand to
                        // `task_result` - the same rule the nag's own line keeps.
                        TodoNeed::Child { id } => format!("the child `{id}`"),
                        // **The job, by its handle**, because that is what the model has to hand
                        // `job_output` or `job_kill` - the same rule the child's id keeps, and the
                        // same rule the nag's own line keeps.
                        TodoNeed::Job { handle } => format!("the job `{handle}`"),
                        TodoNeed::Monitor { name } => format!("the monitor `{name}`"),
                        // **The room's item, by the number `room`'s own verbs take** - the edge
                        // the daemon's room pass puts on a `you hold …` row while the room is
                        // working on it, so a model reading its list sees why the row is quiet.
                        TodoNeed::Room { item, .. } => format!("the room item `#{item}`"),
                        // **The operator, by the question** - the words they were asked, so the
                        // model can see what it is waiting for and try to work it out itself.
                        TodoNeed::Operator { question, .. } => {
                            format!("the OPERATOR — asked \"{}\"", question.trim())
                        }
                        TodoNeed::Answered { .. } => unreachable!("filtered above"),
                        TodoNeed::Unknown => "a kind this build cannot evaluate".to_string(),
                    })
                    .collect();
                // **And what the operator ANSWERED**, on the row it unblocked: *"todo becomes
                // unblocked with reply attached"*. Said after the waits, so a row that was asked
                // twice reads in order.
                let answered: Vec<String> = t
                    .needs
                    .iter()
                    .filter_map(|need| match need {
                        TodoNeed::Answered { question, reply } => Some(format!(
                            "  (answered: you asked \"{}\" — {})",
                            question.trim(),
                            reply
                        )),
                        _ => None,
                    })
                    .collect();
                let waiting = if named.is_empty() {
                    String::new()
                } else {
                    format!("  (waiting on {})", named.join(", "))
                };
                format!("{waiting}{}", answered.join(""))
            }
        ));
    }
    // And what `[p]` means, because the model has to stop proposing the row without being told
    // twice: a mark with no legend reads as mine to pick up. Said only when such a row exists.
    if todos.iter().any(|t| t.status == TodoStatus::Postponed) {
        out.push_str(
            "\n`[p]` is a row the OPERATOR has set aside — it is still on the board, it is not work \
             you are being asked for, and your own `todo_write` does not lift it: do not propose it \
             again. They lift it themselves when they want it back.\n",
        );
    }
    // And what `[c]` means, one word over: a row the operator struck off stays in the history and
    // is not work, so it must not be proposed, restored or re-worded.
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
