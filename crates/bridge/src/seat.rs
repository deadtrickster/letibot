//! A Claude Code seat's todo board: the board, the waits and the idle nag a session has, held by
//! the bridge because a seat is not a session any daemon serves (`docs/seat-todo.md`).
//!
//! The board is `letibot_tools::builtins::todo`'s own type and the schedule is `TodoNagClock`'s, so
//! a row, a wait and a notice mean one thing on both sides of the bridge. What is here is only what
//! the bridge knows and a daemon does not: that the seat is a process talking MCP, that its packages
//! are its held heads, and that its nag is a channel notification.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_tokencore::store::{Store, TodoItem, TodoNeed, TodoStatus};
use letibot_tools::builtins::jobs::{Found, JobLookup, JobTable};
use letibot_tools::builtins::todo::{
    ChildSessions, ChildTodos, RunningChild, RunningJobs, TodoBoard, TodoNagClock, TodoWriteTool,
    the_plan_as_checked, unfinished_plan,
};
use letibot_tools::exec::procs;
use letibot_tools::runtime::Invocation;
use serde_json::{Value, json};

use crate::head::{Head, State, Turn};
use crate::push::Outbox;
use crate::seat_jobs::SeatJobs;

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
    /// This seat's OWN jobs (`seat_job_start`), which no daemon runs: the bridge does, and the
    /// store keeps their rows under the seat's handle.
    jobs: Arc<SeatJobs>,
    /// The seat's channel: the nags and its own jobs' endings, kept so a client that reconnects is
    /// told what it missed. Created by the first [`Seat::watch`].
    outbox: Mutex<Option<Arc<Outbox>>>,
    /// How many notes this seat has pushed. A nag is said again on the ladder, so its outbox key
    /// has to be new every time - the plan key would silence its own repeat.
    notes: AtomicU64,
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
        let jobs = SeatJobs::new(
            handle.clone(),
            store.clone(),
            crate::daemon::rundir().join(format!(
                "{}.seat-jobs-{}",
                crate::daemon::key(store.parent().unwrap_or(&store)),
                handle.replace(['/', '@', ':'], "-")
            )),
        );
        SeatJobs::reap_in_background(&jobs);
        Seat {
            handle,
            board: Arc::new(board),
            clock: Mutex::new(TodoNagClock::with_window(NAG_AFTER).keyed_by_plan()),
            active: Mutex::new(Instant::now()),
            dirty: AtomicBool::new(true),
            working: Mutex::new(HashMap::new()),
            store,
            heads,
            jobs,
            outbox: Mutex::new(None),
            notes: AtomicU64::new(0),
        }
    }

    /// This seat's own jobs: the handle `seat_job_start` answered is `wait_on`'s `job`, and the
    /// listing is `seat_jobs`.
    pub fn jobs(&self) -> &Arc<SeatJobs> {
        &self.jobs
    }

    /// A seat whose window is shorter than a minute - the tests' door, and `set_window`'s reason:
    /// driving the schedule has to be possible without sleeping through the window it pins.
    pub fn with_window(mut self, window: Duration) -> Seat {
        self.clock = Mutex::new(TodoNagClock::with_window(window).keyed_by_plan());
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

    /// The packages and their open jobs, the seat's OWN jobs, and the processes its rows wait on -
    /// as the plan check reads them, with a signature of the reading: a pass that finds the same
    /// signature does not re-arm and cannot postpone the check.
    fn packages(&self, rows: &[TodoItem]) -> (ChildSessions, RunningJobs, String) {
        let states: Vec<State> = self
            .heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|head| head.snapshot())
            .collect();
        let now = Instant::now();
        let (children, mut jobs, from_heads) = {
            let mut working = self.working.lock().unwrap_or_else(|e| e.into_inner());
            read_packages(&states, &mut working, now)
        };
        let mut signature = format!("v{}", self.board.version());
        signature.push_str(&from_heads);
        // The seat's own jobs: no daemon runs them, so the check names them as this board's own -
        // and a row waiting on one is quiet until the reaper says it ended.
        for job in self.jobs.open() {
            signature.push_str(&format!("|seat:{}", job.handle));
            jobs = jobs
                .running(job.handle.clone())
                .about(job.handle.clone(), job.command.clone());
        }
        // And the processes the board names by pid: the only fact about them is the process table,
        // so it is read here and the pair travels to the narrowing - and into the signature, because
        // an exit is what makes the row askable again.
        for (pid, start) in pid_waits(rows) {
            let up = procs::alive(pid, start);
            signature.push_str(&format!("|pid:{pid}:{up}"));
            if up {
                jobs = jobs.pid(pid, start);
            }
        }
        (children, jobs, signature)
    }

    /// The open jobs of every package and of the seat itself, as the board's `job` waits are
    /// resolved against.
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
        open.extend(self.jobs.open().into_iter().map(|j| j.handle));
        open.sort();
        open.dedup();
        PackageJobs { open }
    }

    /// Push this seat's nags - and its own jobs' endings - on `tx` from now on, watching the board
    /// and the packages while the bridge lives. Called once, when the client has initialized and a
    /// channel frame is legal; a second call only re-points the channel at the new client.
    pub fn watch(seat: &Arc<Seat>, tx: Sender<Value>) {
        let mut outbox = seat.outbox.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(outbox) = outbox.as_ref() {
            outbox.attach(tx);
            return;
        }
        let made = Outbox::at(crate::daemon::rundir().join(format!(
            "{}.seat-nag-{}",
            crate::daemon::key(seat.store.parent().unwrap_or(&seat.store)),
            seat.handle.replace(['/', '@', ':'], "-")
        )));
        made.attach(tx);
        seat.jobs.attach(made.clone());
        *outbox = Some(made.clone());
        drop(outbox);
        let seat = seat.clone();
        let _ = std::thread::Builder::new()
            .name("leticode-seat-todo".into())
            .spawn(move || run(&seat, &made));
    }
}

/// The watcher: the session's schedule over the seat's own idleness. It re-arms when the board or a
/// package moves, or when the seat has called a tool since the last pass, and it asks the clock
/// whether the check is due on every pass - `due_now`'s disarm is before the push, so a delivery
/// that took longer than the window does not fire twice.
fn run(seat: &Arc<Seat>, outbox: &Arc<Outbox>) {
    let mut armed_for = String::new();
    loop {
        let now = Instant::now();
        let rows = seat.board.snapshot();
        let (children, jobs, signature) = seat.packages(&rows);
        let dirty = seat.dirty.swap(false, Ordering::SeqCst);
        let active = *seat.active.lock().unwrap_or_else(|e| e.into_inner());
        let mut clock = seat.clock.lock().unwrap_or_else(|e| e.into_inner());
        let moved = dirty || signature != armed_for;
        armed_for = signature;
        if let Some(notice) = a_pass(&mut clock, &rows, &children, &jobs, moved, active, now)
            && let Some(text) = notice
        {
            // A new key per delivery: a plan said again on the ladder is news, and the outbox's
            // key is what would otherwise silence its own repeat.
            let n = seat.notes.fetch_add(1, Ordering::SeqCst);
            outbox.record(format!("nag:{n}"), nag(seat.handle(), &text));
        }
        drop(clock);
        std::thread::sleep(PASS);
    }
}

/// The packages and their open jobs, out of the heads' snapshots, as the plan check reads them.
/// Split out of [`Seat::packages`] so a test states a package's reading as a value.
///
/// A package is a child WHILE ITS TURN IS IN FLIGHT, which is the daemon's own reading of its
/// task journal: a child that ended is not in the snapshot, and that is what ends the wait. A head
/// this bridge could not read is the exception - the frames it skipped include the turn state, so
/// the package is a child that says `cannot say`, never one that ended.
fn read_packages(
    states: &[State],
    working: &mut HashMap<String, Working>,
    now: Instant,
) -> (ChildSessions, RunningJobs, String) {
    let mut children = ChildSessions::none();
    let mut jobs = RunningJobs::none();
    let mut at_work: HashSet<String> = HashSet::new();
    let mut signature = String::new();
    for state in states {
        let running = matches!(state.turn, Turn::Running);
        let unknown = state.unreadable_word();
        // The trust flag is half the signature: a head that starts or stops being readable is a
        // change in the plan's reading, so the check must re-arm on it.
        signature.push_str(&format!(
            "|{}:{running}:{}:{:?}",
            state.session,
            state.is_readable(),
            state.jobs_open
        ));
        for handle in &state.jobs_open {
            jobs = jobs
                .running(handle.clone())
                // A package's job is the PACKAGE's: the check names it as such and offers the
                // package's turn instead (`docs/seat-todo.md`).
                .of_package(handle.clone(), state.session.clone());
        }
        if unknown.is_some() {
            // The job list may be one of the skipped frames, so a handle this reading does not
            // hold may still be running: an absence is not an ending.
            jobs = jobs.uncertain();
        } else if !still_a_child(
            running,
            &state.jobs_open,
            state.asks.len(),
            !state.activity.waiting_on().is_empty(),
        ) {
            continue;
        }
        at_work.insert(state.session.clone());
        let entry = working.entry(state.session.clone()).or_insert(Working {
            since: now,
            last_seq: state.seq,
            last_change: now,
        });
        // A job of its own running is the package's evidence of work, as a new event is.
        if state.seq != entry.last_seq || !state.jobs_open.is_empty() {
            entry.last_seq = state.seq;
            entry.last_change = now;
        }
        children = children.running(
            state.session.clone(),
            RunningChild {
                age: now.saturating_duration_since(entry.since),
                // An unreadable head's silence is this bridge's blindness, not the package's: the
                // gap would be a fact about the reading and not about the work.
                quiet_for: unknown
                    .is_none()
                    .then(|| now.saturating_duration_since(entry.last_change)),
                working: running || !state.jobs_open.is_empty(),
                unknown,
            },
        );
    }
    working.retain(|id, _| at_work.contains(id));
    (children, jobs, signature)
}

/// The processes the board waits on, by pid and start time - the pair an edge on a process
/// carries. Deduplicated, because two rows may wait on one gate.
fn pid_waits(rows: &[TodoItem]) -> Vec<(u32, u64)> {
    let mut out: Vec<(u32, u64)> = rows
        .iter()
        .flat_map(|r| r.needs.iter())
        .filter_map(|n| match n {
            TodoNeed::Pid { pid, start } => Some((*pid, *start)),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// One pass of the watcher, as a value: `None` when the check is not due, else what it said - the
/// notice, or `None` for a delivery with nothing left to ask about. Split out of [`run`] so a test
/// can drive twenty passes over one plan without a daemon to hold the packages.
fn a_pass(
    clock: &mut TodoNagClock,
    rows: &[TodoItem],
    children: &ChildSessions,
    jobs: &RunningJobs,
    moved: bool,
    active: Instant,
    now: Instant,
) -> Option<Option<String>> {
    if moved {
        // `rearm_from` and not `rearm`: a seat that is still calling tools is working, so the
        // deadline moves with its last call - a session's, by contrast, must not be postponed
        // by a monitor waking it.
        clock.rearm_from(rows, active, children, jobs);
    }
    if !clock.due_now(now) {
        return None;
    }
    let askable = the_plan_as_checked(rows, children, jobs);
    let notice = unfinished_plan(&askable, rows, children, jobs);
    // Recorded before the push, for `TodoNagClock::said`'s reason: a notice whose write failed
    // still counts as said, or it would come back at every pass. Recorded by the PLAN, so that the
    // packages' jobs coming and going are not twenty reasons to ask one plan again.
    clock.said_plan(&askable, notice.clone(), now);
    Some(notice)
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

/// A package is still the seat's child while its turn runs, while it waits on a job of its own
/// between turns, while it holds an ask, or while the DAEMON says it is waiting on anything. The
/// last is not the same as an open job: `waiting_on` is the job watcher's answer and `jobs_open`
/// the process host's, and a job whose completion is queued is at work and not running.
fn still_a_child(
    running: bool,
    jobs_open: &std::collections::BTreeSet<String>,
    asks: usize,
    waiting: bool,
) -> bool {
    running || !jobs_open.is_empty() || asks > 0 || waiting
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::activity::{Activity, Waiting};
    use letibot_tokencore::store::TodoBy;
    use letibot_tools::builtins::todo::{ChildDoing, child_doing, has_room_to_breathe};

    /// A row with no waits, as a seat writes one.
    fn row(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.into(),
            status,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }
    }

    /// A row waiting on a package that sits between turns on its own job stays quiet: the package
    /// has not finished, and the row is not "in progress and waits on nothing".
    #[test]
    fn a_package_waiting_on_its_own_job_between_turns_is_still_a_child() {
        let jobs: std::collections::BTreeSet<String> = ["j84628".to_string()].into();
        let none = std::collections::BTreeSet::new();
        assert!(
            still_a_child(false, &jobs, 0, false),
            "idle on a job of its own"
        );
        assert!(still_a_child(false, &none, 1, false), "idle on an ask");
        assert!(
            still_a_child(false, &none, 0, true),
            "the worker sitting on something IS the fact"
        );
        assert!(still_a_child(true, &none, 0, false), "mid-turn");
        assert!(
            !still_a_child(false, &none, 0, false),
            "idle with nothing open has ended"
        );

        let window = Duration::from_millis(50);
        let mut clock = TodoNagClock::with_window(window).keyed_by_plan();
        let mut waiting = row("K2 package", TodoStatus::InProgress);
        waiting.needs = vec![TodoNeed::Child { id: "K2".into() }];
        let rows = vec![waiting];
        let between_turns = ChildSessions::none().running(
            "K2",
            RunningChild {
                age: Duration::from_secs(600),
                quiet_for: Some(Duration::from_secs(30)),
                working: false,
                unknown: None,
            },
        );
        let jobs = RunningJobs::none()
            .running("j84628".to_string())
            .of_package("j84628".to_string(), "K2");
        let t0 = Instant::now();
        let said = a_pass(
            &mut clock,
            &rows,
            &between_turns,
            &jobs,
            true,
            t0,
            t0 + window,
        );
        assert!(
            !matches!(&said, Some(Some(text)) if text.contains("waits on nothing")),
            "{said:?}"
        );
    }

    /// Twenty package jobs starting and ending over one unchanged seat plan are ONE check: the
    /// notice names them, so the sentence moves, and the plan it is about does not.
    #[test]
    fn twenty_package_jobs_starting_and_ending_say_one_unchanged_plan_once() {
        let window = Duration::from_millis(200);
        let mut clock = TodoNagClock::with_window(window).keyed_by_plan();
        let rows = vec![row("ship the pane", TodoStatus::InProgress)];
        let packages = ChildSessions::none().running(
            "K1",
            RunningChild {
                age: Duration::from_secs(60),
                quiet_for: Some(Duration::from_secs(1)),
                working: true,
                unknown: None,
            },
        );
        let t0 = Instant::now();
        let first = a_pass(
            &mut clock,
            &rows,
            &packages,
            &RunningJobs::none(),
            true,
            t0,
            t0 + window,
        )
        .expect("the check is due")
        .expect("there is work to name");
        assert!(first.contains("ship the pane"), "{first}");

        for n in 0..20 {
            let handle = format!("j{}", 83500 + n);
            let jobs = RunningJobs::none()
                .running(handle.clone())
                .of_package(handle, "K1");
            let now = t0 + window + Duration::from_millis(1 + n);
            assert_eq!(
                a_pass(&mut clock, &rows, &packages, &jobs, true, t0, now),
                None,
                "a package's job starting or ending said the seat's plan again"
            );
        }

        // And a plan that MOVES is news at once - the row that was only in progress gains a
        // second one beside it.
        let grown = vec![
            row("ship the pane", TodoStatus::InProgress),
            row("and the thing after it", TodoStatus::Pending),
        ];
        assert!(
            a_pass(
                &mut clock,
                &grown,
                &packages,
                &RunningJobs::none(),
                true,
                t0,
                t0 + window + Duration::from_millis(30)
            )
            .is_some(),
            "a plan that moved is asked about"
        );
    }

    /// A head that is on another protocol number, or that skipped frames this build could not
    /// read, is not a head that ended its turn: its package is still a child, and a row waiting on
    /// it is never told it waits on nothing.
    #[test]
    fn a_package_whose_head_could_not_be_read_is_never_read_as_idle() {
        let window = Duration::from_millis(50);
        let t0 = Instant::now();
        let mut skewed = State::of("s-pkg");
        skewed.skew = Some("this bridge speaks 67 and the daemon 68".into());
        let mut skipped = State::of("s-pkg");
        skipped.unreadable = 3;
        for mut state in [skewed, skipped] {
            state.turn = Turn::Idle;
            let mut working = HashMap::new();
            let (children, jobs, _) = read_packages(std::slice::from_ref(&state), &mut working, t0);
            let package = children
                .child("s-pkg")
                .unwrap_or_else(|| panic!("{:?} read a working package as ended", state.skew));
            assert!(package.unknown.is_some(), "and says why");
            assert_eq!(child_doing(package), ChildDoing::Unknown);
            assert!(
                has_room_to_breathe(package),
                "nothing here may steer a child it cannot read"
            );

            let mut waiting = row("R162 package the nags", TodoStatus::InProgress);
            waiting.needs = vec![TodoNeed::Child { id: "s-pkg".into() }];
            let rows = vec![waiting];
            let mut clock = TodoNagClock::with_window(window).keyed_by_plan();
            let said = a_pass(&mut clock, &rows, &children, &jobs, true, t0, t0 + window);
            assert!(
                !matches!(&said, Some(Some(text)) if text.contains("waits on nothing")),
                "a package nobody could read is not a wait that ended: {said:?}"
            );
            assert!(said.is_none(), "so the check says nothing: {said:?}");
        }
    }

    /// A row with several waits is quiet while any one of them is still running. Its dead pid and
    /// its ended job say nothing about the third, and a handle missing from a snapshot that skipped
    /// frames is unknown - not ended.
    #[test]
    fn a_row_with_several_waits_is_quiet_until_the_last_one_ends() {
        let window = Duration::from_millis(50);
        let t0 = Instant::now();
        let mut waited = row("the soak, the gate and the build", TodoStatus::InProgress);
        waited.needs = vec![
            TodoNeed::Pid {
                pid: 4_000_000,
                start: 1,
            },
            TodoNeed::Job {
                handle: "j1".into(),
            },
            TodoNeed::Job {
                handle: "j2".into(),
            },
        ];
        let rows = vec![waited];
        let mut clock = TodoNagClock::with_window(window).keyed_by_plan();
        let mut working = HashMap::new();

        // The head is skewed, so the job list the seat reads never arrived: j2 is running and
        // invisible, and an absence is not an ending.
        let mut skewed = State::of("s-pkg");
        skewed.skew = Some("this bridge speaks 67 and the daemon 68".into());
        let (children, jobs, _) = read_packages(std::slice::from_ref(&skewed), &mut working, t0);
        assert!(jobs.is_uncertain(), "a list that was skipped is not a list");
        let said = a_pass(&mut clock, &rows, &children, &jobs, true, t0, t0 + window);
        assert!(
            !matches!(&said, Some(Some(text)) if text.contains("the soak")),
            "a wait nobody could read was taken for one that ended: {said:?}"
        );

        // The head reads cleanly and lists j2: still quiet, and for the job's own reason.
        let mut clean = State::of("s-pkg");
        clean.jobs_open.insert("j2".into());
        let (children, jobs, _) = read_packages(std::slice::from_ref(&clean), &mut working, t0);
        assert!(jobs.is_running("j2"));
        let said = a_pass(&mut clock, &rows, &children, &jobs, true, t0, t0 + window);
        assert!(
            !matches!(&said, Some(Some(text)) if text.contains("the soak")),
            "the last one is still running: {said:?}"
        );

        // j2 ends, and now the LAST of the row's waits has ended: the row is work, and asked about.
        let over = State::of("s-pkg");
        let (children, jobs, _) = read_packages(std::slice::from_ref(&over), &mut working, t0);
        let said = a_pass(&mut clock, &rows, &children, &jobs, true, t0, t0 + window);
        assert!(
            matches!(&said, Some(Some(text)) if text.contains("the soak, the gate and the build")),
            "every wait has ended, so the row is work: {said:?}"
        );
    }

    /// Seen live with no protocol skew: a package between turns reported `jobs_open: []` while its
    /// `waiting_on` named a job and a child and its activity was busy. The daemon's own answer is
    /// the last word - a package the worker is sitting on has not finished.
    #[test]
    fn a_package_between_turns_the_daemon_says_it_is_waiting_on_is_still_a_child() {
        let window = Duration::from_millis(50);
        let t0 = Instant::now();
        let mut state = State::of("s-1794-pkg");
        // The snapshot as the seat read it: nothing in the job table, nothing asked, and the
        // daemon saying it is at work on a job and a child.
        state.activity = Activity::of(vec![
            Waiting::Job("j85996".into()),
            Waiting::Child("s-1794-sub-1".into()),
        ]);
        assert!(state.jobs_open.is_empty(), "the host's table held nothing");
        assert!(state.asks.is_empty(), "and nothing was asked");
        assert!(matches!(state.turn, Turn::Idle), "between turns");

        let mut working = HashMap::new();
        let (children, jobs, _) = read_packages(std::slice::from_ref(&state), &mut working, t0);
        let package = children
            .child("s-1794-pkg")
            .expect("a package the worker is sitting on has not finished");
        assert!(
            package.unknown.is_none(),
            "no skew here: this is the daemon's word"
        );
        assert!(
            has_room_to_breathe(package),
            "and it is out of the check's hands"
        );

        let mut waiting = row("R162 package the nags", TodoStatus::InProgress);
        waiting.needs = vec![TodoNeed::Child {
            id: "s-1794-pkg".into(),
        }];
        let rows = vec![waiting];
        let mut clock = TodoNagClock::with_window(window).keyed_by_plan();
        let said = a_pass(&mut clock, &rows, &children, &jobs, true, t0, t0 + window);
        assert!(
            said.is_none(),
            "a package still at work is not a wait that ended: {said:?}"
        );
    }
}
