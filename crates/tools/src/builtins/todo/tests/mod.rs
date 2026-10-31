//! **The todo tool's tests**, one file per theme, and the fixtures they share. Each theme file
//! reaches the fixtures, the tool and the imports below through `use super::*`.

use super::*;

use crate::backend::HostBackend;

use crate::backend::tempdir::TempDir;

use crate::events::RecordingToolSink;

use crate::runtime::{Registry, ToolRuntime};

use letibot_transcript::{ToolCall, ToolOutcome};

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

// -- the turn boundary ------------------------------------------------

/// **The check's sentence for a plan that IS the whole board** — the shorthand every test below
/// uses, and the honest one: these plans carry no POSTPONED row, so the caller's narrowing
/// (`the_plan_as_checked`) would hand back exactly the same rows, and no row of theirs waits on
/// a child or on a job, so `ChildSessions::none()` and `RunningJobs::none()` are the reading the
/// daemon would hand in for them. A test that needs the two to differ says so by calling
/// `unfinished_plan` with both — or by passing the children or the jobs it means, as the child
/// and job tests below do.
fn nag_for(todos: &[TodoItem]) -> Option<String> {
    nag_with(todos, &ChildSessions::none(), &no_jobs())
}

/// [`nag_for`] with the daemon's own reading of the children a plan waits on.
fn nag_with(todos: &[TodoItem], children: &ChildSessions, jobs: &RunningJobs) -> Option<String> {
    unfinished_plan(todos, todos, children, jobs)
}

/// **Nothing running outside the board** — no job this session holds, which is the reading every
/// test here that is not about a job passes, and the one a daemon with no process host hands in.
fn no_jobs() -> RunningJobs {
    RunningJobs::none()
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

/// **A row that waits on a JOB** — the case this feature was asked for: work that cannot start
/// until something the daemon is running has ended.
fn waiting_on_a_job(content: &str, handle: &str) -> TodoItem {
    TodoItem {
        needs: vec![TodoNeed::Job {
            handle: handle.to_string(),
        }],
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
        // The board's own writer, so its title-cap refusal reaches the tool as the real
        // resolver's does: `?` and nothing else.
        self.board.upsert_parent(rows, &by)?;
        Ok(self.board.snapshot())
    }

    /// One child of this session, `s-1-sub-2`, and nothing else — the parentage the real
    /// resolver checks, reduced to the one answer each test needs.
    fn own_child(&self, asked: &str) -> Result<String, String> {
        // The child's name resolves to it too — the real resolver's slug arm, reduced.
        if asked == "s-1-sub-2" || asked == "fix-login" {
            Ok("s-1-sub-2".to_string())
        } else {
            Err(format!("`{asked}` is not one of this session's children"))
        }
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

mod board;
mod cap;
mod children;
mod nag;
mod needs;
mod operator;
mod quotes;
mod r93;
mod room;
mod wait_on;
