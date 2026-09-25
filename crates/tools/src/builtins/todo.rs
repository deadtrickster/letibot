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

use letibot_tokencore::store::{TodoBy, TodoItem, TodoStatus};
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
    version: AtomicU64,
}

impl TodoBoard {
    /// Start with a list — the store's, when this session was resumed, so the
    /// plan the model was working from is what it keeps working from.
    pub fn new(initial: Vec<TodoItem>) -> Self {
        TodoBoard {
            todos: Mutex::new(initial),
            operator: Mutex::new(Vec::new()),
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

    /// The operator's half alone, for a caller that needs to tell the two apart.
    pub fn operator_snapshot(&self) -> Vec<TodoItem> {
        self.operator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replace the list. Returns the new version, which is what the harness
    /// compares against to decide whether a store write and an announcement are
    /// owed.
    pub fn replace(&self, todos: Vec<TodoItem>) -> u64 {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = todos;
        self.version.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The list as it stands.
    pub fn snapshot(&self) -> Vec<TodoItem> {
        // **THE UNION, and it is the whole of the feature.** Everything downstream reads this one
        // getter — the pane's `Todos` reply, the model's own view of the plan, and the idle nag
        // (`harness.rs`'s `nag_notice`, which asks `unfinished_plan` of exactly this) — so nothing
        // else had to learn that the operator can write a row too. The model's list comes first
        // because it is the list the model has been working from, and the operator's rows are the
        // ones it has been asked for on top.
        let mut out = self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
pub fn unfinished_plan(todos: &[TodoItem]) -> Option<String> {
    let open: Vec<&TodoItem> = todos
        .iter()
        .filter(|t| t.status != TodoStatus::Completed)
        .collect();
    if open.is_empty() {
        return None;
    }
    // **ONE ITEM, and the operator's reason is the model's own behaviour:** *"i think the nagger should
    // mention only one todo at a time, so a model will not be defocused."*
    //
    // A list invites a model to touch all of it: it reads five open rows, does a little of each, and
    // ends the next turn with five still open — which is the failure this check exists to prevent,
    // arriving on the check's own message. Naming ONE is a directive; naming five is homework.
    //
    // **WHICH one, and it is not simply the first.** An item the model marked `in_progress` is the one
    // it told the board it was doing, so that is the honest thing to name — an item it never started
    // is a plan it has not got to, and interrupting that with a different row would be the nag
    // choosing the model's next step. `in_progress` first, then the earliest open row, and the
    // ordering within each is the list's own (the operator's rows come after the model's, so a
    // model that has started nothing is pointed at its own plan before the operator's).
    let next = open
        .iter()
        .find(|t| t.status == TodoStatus::InProgress)
        .or_else(|| open.first())
        .expect("open is not empty");
    let left = open.len() - 1;
    // The count of what is BEHIND this one, because the model is entitled to know the plan is bigger
    // than the row it is being asked about — and that is exactly the fact that must not become a list.
    let rest = match left {
        0 => String::new(),
        1 => " (1 more open)".to_string(),
        n => format!(" ({n} more open)"),
    };
    let state = match next.status {
        TodoStatus::InProgress => " — you had this one in progress",
        _ => "",
    };
    Some(format!(
        "[todo check] this turn is finished and one item is not done{rest}:\n  - {}{state}\n{}",
        next.content.trim(),
        "do this one, or mark it done, or drop it — a plan left open is a plan nobody is \
         following. If you are stopping here deliberately, say why in your reply."
    ))
}

// `MAX_PLAN_LINES` is gone with the list it capped: the message names ONE item now, so there is no
// length of list to truncate. The operator's ruling — *"only one todo at a time, so a model will not
// be defocused"* — removes the thing that constant existed for.

/// The write tool. Holds the board; the harness holds the same `Arc`.
pub struct TodoWriteTool {
    board: Arc<TodoBoard>,
}

impl TodoWriteTool {
    pub fn new(board: Arc<TodoBoard>) -> Self {
        TodoWriteTool { board }
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
             omitting an entry removes it.",
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The complete list, in the order to do them.",
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
        let mut items = Vec::with_capacity(list.len());
        for (i, t) in list.iter().enumerate() {
            let Some(content) = t.get("content").and_then(|v| v.as_str()) else {
                return Invocation::failed(
                    format!("entry {} has no content", i + 1),
                    "every entry needs `content` (what the step is) and `status`.",
                );
            };
            if content.trim().is_empty() {
                return Invocation::failed(
                    format!("entry {} is empty", i + 1),
                    "an empty entry says nothing; drop it or write the step.",
                );
            }
            let status = match t.get("status").and_then(|v| v.as_str()) {
                Some("pending") => TodoStatus::Pending,
                Some("in_progress") => TodoStatus::InProgress,
                Some("completed") => TodoStatus::Completed,
                Some(other) => {
                    return Invocation::failed(
                        format!("entry {} has status `{other}`", i + 1),
                        "`status` is one of: pending, in_progress, completed.",
                    );
                }
                None => {
                    return Invocation::failed(
                        format!("entry {} has no status", i + 1),
                        "every entry needs `content` and `status`.",
                    );
                }
            };
            items.push(TodoItem {
                content: content.to_string(),
                status,
                // the MODEL's list, by definition: this function is the `todo` tool
                by: letibot_tokencore::store::TodoBy::Model,
            });
        }
        self.board.replace(items);
        // The list back to the model, as it now stands — so the next call is
        // written against what the pane shows, not against what the model
        // believes it wrote.
        Invocation::ok(render(&self.board.snapshot()))
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
        };
        out.push_str(&format!("  {}. {} {}\n", i + 1, mark, t.content));
    }
    out
}

#[cfg(test)]
mod tests {
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
        let mut b = TodoBoard::new(vec![TodoItem {
            content: "the model's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
        }]);
        assert_eq!(b.snapshot().len(), 1, "the model's list as given");

        b.set_operator(vec![TodoItem {
            content: "the operator's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
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
        }]);
        let after = b.snapshot();
        assert_eq!(after.len(), 2, "the operator's row survived: {after:?}");
        assert!(after.iter().any(|t| t.content == "the operator's"));
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
            msg.contains("you had this one in progress"),
            "and the message says why this one: {msg}"
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
}
