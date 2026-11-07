//! A Claude Code seat's todo board: the board, the waits and the idle nag a session has, held by
//! the bridge because a seat is not a session any daemon serves (`docs/seat-todo.md`).
//!
//! The board is `letibot_tools::builtins::todo`'s own type and the schedule is `TodoNagClock`'s, so
//! a row, a wait and a notice mean one thing on both sides of the bridge. What is here is only what
//! the bridge knows and a daemon does not: that the seat is a process talking MCP, that its packages
//! are its held heads, and that its nag is a channel notification.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_tokencore::store::{Store, TodoItem, TodoNeed, TodoStatus};
use letibot_tools::builtins::jobs::{Found, JobLookup, JobTable};
use letibot_tools::builtins::todo::{
    ChildSessions, ChildTodos, RunningChild, RunningJobs, TodoBoard, TodoNagClock, TodoWriteTool,
    the_plan_as_checked, unfinished_plan,
};
use letibot_tools::runtime::Invocation;
use serde_json::{Value, json};

use crate::head::{Head, Turn};

/// How long the seat sits quiet before the plan check comes due: the session's own window
/// (`TODO_NAG_AFTER`), because a seat that has not called a tool is as idle as a turn that ended.
pub const NAG_AFTER: Duration = Duration::from_secs(60);

/// How often the watcher reads the board and the packages. Not the window: the nag is still once
/// per change, and this is only how long a change or a deadline may sit unseen.
const PASS: Duration = Duration::from_millis(250);

/// A package's turn as this seat has watched it: when it went to work, and the last thing it said.
struct Working {
    since: Instant,
    last_seq: u64,
    last_change: Instant,
}

/// One seat's board. `Bridge` owns one, lazily, and hands its `Arc` to every call that needs it.
pub struct Seat {
    /// The handle the board is stored under and the nag is addressed from: `cc@<project>`.
    handle: String,
    board: Arc<TodoBoard>,
    clock: Mutex<TodoNagClock>,
    /// When the seat last called a tool. The seat's turn is over at that instant, so the window is
    /// measured from here and a call postpones a check the seat could not have answered yet.
    active: Mutex<Instant>,
    /// A tool call since the last pass: re-arm from `active`, even if the board did not move.
    dirty: AtomicBool,
    /// Each held head's turn, by session id.
    working: Mutex<HashMap<String, Working>>,
    /// The store the board is written to and read from, and where its last write left it.
    store: PathBuf,
    /// The bridge's packages, shared and not copied: a head held later is a package here at once.
    heads: Arc<Mutex<HashMap<String, Arc<Head>>>>,
}

impl Seat {
    /// A seat whose board is read from `store` under `handle`, checked against `heads`.
    pub fn new(
        handle: impl Into<String>,
        store: impl Into<PathBuf>,
        heads: Arc<Mutex<HashMap<String, Arc<Head>>>>,
    ) -> Seat {
        let handle = handle.into();
        let store = store.into();
        let board = TodoBoard::new(load(&store, &handle));
        Seat {
            handle,
            board: Arc::new(board),
            clock: Mutex::new(TodoNagClock::with_window(NAG_AFTER)),
            active: Mutex::new(Instant::now()),
            dirty: AtomicBool::new(true),
            working: Mutex::new(HashMap::new()),
            store,
            heads,
        }
    }

    /// A seat whose window is shorter than a minute - the tests' door, and `set_window`'s reason:
    /// driving the schedule has to be possible without sleeping through the window it pins.
    pub fn with_window(mut self, window: Duration) -> Seat {
        self.clock = Mutex::new(TodoNagClock::with_window(window));
        self
    }

    pub fn handle(&self) -> &str {
        &self.handle
    }

    /// The board as it stands, in the shape the store holds it.
    pub fn rows(&self) -> Vec<TodoItem> {
        self.board.snapshot()
    }

    /// The board as a tool answers it: every row with its waits, and the version they last moved at.
    pub fn board_json(&self) -> Value {
        json!({
            "seat": self.handle,
            "rows": self.board.snapshot(),
            "version": self.board.version(),
        })
    }

    /// The seat called a tool: it is not idle, and a deadline already set moves to the end of the
    /// window from here. What this does NOT do is forget a notice already delivered - a seat that
    /// answers by calling a tool and changing nothing has not changed the plan.
    pub fn touch(&self) {
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// `todo_write`, run through the session's own tool so the refusals, the row bounds, the
    /// `wait_on` shapes and the reply are the ones a leitcode session gets.
    pub fn write(&self, args: &Value) -> Result<Value, String> {
        let tool = TodoWriteTool::with_children(
            self.board.clone(),
            Arc::new(Packages {
                heads: self.heads.clone(),
            }),
        );
        let jobs = self.open_jobs();
        let invocation = tool.write(args, Some(&jobs));
        self.settled(invocation)
    }

    /// The operator's reply to a row's question, recorded on the row as the words they used. A seat
    /// has no card to offer options on, so there is nothing here to validate but the row and the
    /// question.
    pub fn answer(&self, args: &Value) -> Result<Value, String> {
        let content = arg(args, "content")?;
        let question = arg(args, "question")?;
        let reply = arg(args, "reply")?;
        self.board
            .answer_operator(&content, &question, &reply)
            .map_err(|why| format!("nothing was written: {why}"))?;
        Ok(self.persisted(json!({
            "said": format!(
                "the operator's answer is on `{}`: the row no longer waits on them.\n\n{}",
                content.trim(),
                render(&self.board.snapshot())
            ),
        })))
    }

    /// What a write leaves behind: the board on disk, the clock re-armed, and the tool's own reply.
    fn settled(&self, invocation: Invocation) -> Result<Value, String> {
        use letibot_transcript::ToolOutcome;
        // A refusal is the tool's, and BOTH halves travel: the reason names the argument that is
        // wrong, the payload says what nothing-was-written means. The MCP layer turns this into
        // `isError`, which is what a failed tool call is.
        let refusal = match &invocation.outcome {
            ToolOutcome::Failed { reason } => Some(reason.clone()),
            ToolOutcome::NotRun { why } => Some(why.clone()),
            _ => None,
        };
        if let Some(refusal) = refusal {
            return Err(format!("{refusal}\n\n{}", invocation.payload));
        }
        Ok(self.persisted(json!({ "said": invocation.payload })))
    }

    /// The answer to a call that changed the board: what it said, the rows as they now are, and
    /// whether the store took them - a board that could not be written is said out loud rather than
    /// silently living in one process's memory.
    fn persisted(&self, mut answer: Value) -> Value {
        let stored = opened(&self.store)
            .and_then(|s| {
                s.put_seat_todos(&self.handle, &self.board.snapshot())
                    .map_err(|e| e.to_string())
            })
            .err();
        answer["rows"] = self.board_json()["rows"].clone();
        answer["version"] = json!(self.board.version());
        answer["stored"] = json!(stored.is_none());
        if let Some(why) = stored {
            answer["store_error"] = json!(format!(
                "the board is in this process only until the store can be written: {why}"
            ));
        }
        self.touch();
        answer
    }

    /// The packages and their open jobs, as the plan check reads them, with a signature of the
    /// reading: a pass that finds the same signature does not re-arm and cannot postpone the check.
    fn packages(&self) -> (ChildSessions, RunningJobs, String) {
        let heads: Vec<Arc<Head>> = self
            .heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        let now = Instant::now();
        let mut children = ChildSessions::none();
        let mut jobs = RunningJobs::none();
        let mut working = self.working.lock().unwrap_or_else(|e| e.into_inner());
        let mut at_work: HashSet<String> = HashSet::new();
        let mut signature = format!("v{}", self.board.version());
        for head in heads {
            let state = head.snapshot();
            let running = matches!(state.turn, Turn::Running);
            // A package is a child WHILE ITS TURN IS IN FLIGHT, which is the daemon's own reading of
            // its task journal: a child that ended is not in the snapshot, and that is what ends the
            // wait. `jobs_open` is what can be waited on beside it.
            signature.push_str(&format!(
                "|{}:{running}:{:?}",
                head.session, state.jobs_open
            ));
            for handle in &state.jobs_open {
                jobs = jobs.running(handle.clone());
            }
            if !running {
                continue;
            }
            at_work.insert(head.session.clone());
            let entry = working.entry(head.session.clone()).or_insert(Working {
                since: now,
                last_seq: state.seq,
                last_change: now,
            });
            if state.seq != entry.last_seq {
                entry.last_seq = state.seq;
                entry.last_change = now;
            }
            children = children.running(
                head.session.clone(),
                RunningChild {
                    age: now.saturating_duration_since(entry.since),
                    quiet_for: Some(now.saturating_duration_since(entry.last_change)),
                    working: true,
                },
            );
        }
        working.retain(|id, _| at_work.contains(id));
        (children, jobs, signature)
    }

    /// The open jobs of every package, as the board's `job` waits are resolved against.
    fn open_jobs(&self) -> PackageJobs {
        let heads: Vec<Arc<Head>> = self
            .heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        let mut open: Vec<String> = Vec::new();
        for head in heads {
            open.extend(head.snapshot().jobs_open);
        }
        open.sort();
        open.dedup();
        PackageJobs { open }
    }

    /// Push this seat's nags on `tx` from now on, watching the board and the packages while the
    /// bridge lives. Called once, when the client has initialized and a channel frame is legal.
    pub fn watch(seat: &Arc<Seat>, tx: Sender<Value>) {
        let seat = seat.clone();
        let _ = std::thread::Builder::new()
            .name("leticode-seat-todo".into())
            .spawn(move || run(&seat, &tx));
    }
}

/// The watcher: the session's schedule over the seat's own idleness. It re-arms when the board or a
/// package moves, or when the seat has called a tool since the last pass, and it asks the clock
/// whether the check is due on every pass - `due_now`'s disarm is before the push, so a delivery
/// that took longer than the window does not fire twice.
fn run(seat: &Arc<Seat>, tx: &Sender<Value>) {
    let mut armed_for = String::new();
    loop {
        let now = Instant::now();
        let (children, jobs, signature) = seat.packages();
        let rows = seat.board.snapshot();
        let dirty = seat.dirty.swap(false, Ordering::SeqCst);
        let mut clock = seat.clock.lock().unwrap_or_else(|e| e.into_inner());
        if dirty || signature != armed_for {
            armed_for = signature;
            let active = *seat.active.lock().unwrap_or_else(|e| e.into_inner());
            // `rearm_from` and not `rearm`: a seat that is still calling tools is working, so the
            // deadline moves with its last call - a session's, by contrast, must not be postponed
            // by a monitor waking it.
            clock.rearm_from(&rows, active, &children, &jobs);
        }
        if clock.due_now(now) {
            let notice = unfinished_plan(
                &the_plan_as_checked(&rows, &children, &jobs),
                &rows,
                &children,
                &jobs,
            );
            // Recorded before the push, for `TodoNagClock::said`'s reason: a notice whose write
            // failed still counts as said, or it would come back at every pass.
            clock.said(notice.clone(), now);
            if let Some(text) = notice {
                let note = nag(seat.handle(), &text);
                if tx.send(note).is_err() {
                    return;
                }
            }
        }
        drop(clock);
        std::thread::sleep(PASS);
    }
}

/// The channel notification for one notice: the same frame a held session's events use, so a client
/// reads one kind of event and `meta.event` says which.
pub fn nag(seat: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": text,
            "meta": { "source": "seat", "event": "todo", "seat": seat },
        },
    })
}

/// The seat's packages as the tool's child resolver: the sessions this bridge holds. A seat writes
/// its own board and no other, so a `target` write is refused rather than aimed at a package.
struct Packages {
    heads: Arc<Mutex<HashMap<String, Arc<Head>>>>,
}

impl ChildTodos for Packages {
    fn upsert_child(
        &self,
        target: &str,
        _rows: &[(String, TodoStatus, Vec<TodoNeed>)],
    ) -> Result<Vec<TodoItem>, String> {
        Err(format!(
            "`{target}` is a package of this seat, and a seat writes its own board: a package's rows \
             belong to the package's session, which has its own `todo_write`. Send the row on your \
             own board and mark it waiting on `{target}` with `wait_on`"
        ))
    }

    fn own_child(&self, asked: &str) -> Result<String, String> {
        let heads = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        if heads.contains_key(asked) {
            return Ok(asked.to_string());
        }
        let mut held: Vec<&str> = heads.keys().map(String::as_str).collect();
        held.sort();
        Err(if held.is_empty() {
            format!("`{asked}` is not a package of this seat: this bridge holds no session")
        } else {
            format!(
                "`{asked}` is not a package of this seat. It holds {}: a wait on a session this \
                 bridge does not hold would never end here",
                held.iter()
                    .map(|h| format!("`{h}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }
}

/// The open jobs of the seat's packages, as `TodoWriteTool`'s job table. The bridge holds handles
/// and no names, so a handle this table does not hold is kept - it may be one a package ran before
/// this bridge started - exactly as a process host's table keeps one from before a restart.
#[derive(Default)]
pub struct PackageJobs {
    open: Vec<String>,
}

impl PackageJobs {
    fn is_open(&self, asked: &str) -> bool {
        self.open.iter().any(|h| h == asked)
    }
}

impl JobTable for PackageJobs {
    fn look(&self, asked: &str) -> Result<Found, JobLookup> {
        // Only a package's OPEN jobs are here, so a handle it holds is a job still running: the
        // bridge has no name for a job, and no table of the ones a daemon has settled.
        match self.open.iter().find(|h| *h == asked) {
            Some(handle) => Ok(Found {
                handle: handle.clone(),
                running: true,
                word: "running".into(),
            }),
            None => Err(JobLookup::Unknown),
        }
    }

    fn knows(&self, name: &str) -> bool {
        self.is_open(name)
    }

    fn unknown(&self, asked: &str) -> Invocation {
        let have = if self.open.is_empty() {
            "no package of this seat has a job running".to_string()
        } else {
            format!(
                "the open jobs are {}",
                self.open
                    .iter()
                    .map(|h| format!("`{h}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Invocation::failed(
            format!("no job called `{asked}`"),
            format!(
                "{have}. A package's job is named by the handle `leticode_status` lists in its \
                 `jobs_open` - the `j…` the daemon minted."
            ),
        )
    }

    fn several(&self, asked: &str) -> Invocation {
        // A handle names one job by construction, so this is the unknown case under another word.
        self.unknown(asked)
    }
}

/// A string argument, or a refusal naming the field.
fn arg(args: &Value, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(format!("`{key}` is empty")),
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("`{key}` is required")),
    }
}

/// The board on disk, or nothing. A store that does not exist is not created here: it is the
/// daemon's file, and a bridge that made one would be a second writer in a folder with no daemon.
fn load(store: &std::path::Path, seat: &str) -> Vec<TodoItem> {
    if !store.exists() {
        return Vec::new();
    }
    Store::open(store)
        .ok()
        .and_then(|s| s.seat_todos(seat).ok())
        .unwrap_or_default()
}

/// The store the board is kept in, or the sentence a caller needs: a bridge started before any
/// daemon has no file yet, and `leticode_start` is what makes one.
fn opened(store: &std::path::Path) -> Result<Store, String> {
    if !store.exists() {
        return Err(format!(
            "there is no store at {} yet: it is the daemon's file, so `leticode_start` is what \
             opens one",
            store.display()
        ));
    }
    Store::open(store).map_err(|e| format!("{}: {e}", store.display()))
}

/// The board as one sentence per row, for an answer that changed nothing but an edge: the shape the
/// tool's own reply uses, so a reader of either sees the same list.
fn render(rows: &[TodoItem]) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        // The wire's own words, so a row reads the same here as it does in the store.
        let word = match row.status {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Postponed => "postponed",
            TodoStatus::Cancelled => "cancelled",
        };
        out.push_str(&format!("{}. [{word}] {}\n", i + 1, row.content));
    }
    out
}
