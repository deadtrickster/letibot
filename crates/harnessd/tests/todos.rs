//! The todo tool's harness half: the board is restored from the store at open.
//!
//! The tool's own behaviour is tested in `letibot-tools` (`todo_write` replaces
//! the whole list, refuses by name, moves the board's version). The store's
//! round-trip is tested in `letibot-tokencore`. What only *this* crate can assert
//! is the seam between them: a harness opened over a session whose model wrote
//! todos comes back with those todos on its board — a resume that opened on an
//! empty plan would send the model back to re-deriving work it had already
//! organised, which is the exact waste the list exists to prevent.
//!
//! It needs the vocabulary GGUF and **not** the model server. `LETIBOT_VOCAB_GGUF`
//! overrides the path.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoItem, TodoStatus};

const WANTED: Dialect = Dialect::Qwen;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // `Config::for_this_box` carries no vocabulary default any more (a daemon on
    // the byte vocabulary needs none), so a harness built here is handed one: the
    // operator's `LETIBOT_VOCAB_GGUF` if it is set, else the box's own GGUF — the
    // same path this file's `present_gguf` gate consults.
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn opened(cfg: &Config, parts: &Parts) -> Harness {
    let hub = Hub::new(&cfg.session_id);
    Harness::open(parts, cfg.clone(), hub).expect("the session must open")
}

#[test]
fn a_resume_comes_back_with_the_plan_the_model_was_working_from() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-todos");
    let path = dir.path().join("sessions.db");
    let session_id = "todos-restore-test";
    {
        // A previous run: the session exists and its model wrote a plan. The
        // session row first — the todo row carries a foreign key to it, which is
        // what makes an orphan list unrepresentable.
        let s = Store::open(&path).expect("seeding");
        s.put_session(&SessionRecord {
            id: session_id.into(),
            title: Some("the todos session".into()),
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            // A root that exists: `Harness::open` validates the workspace before
            // it reads anything else, and `/w` was the failure, not the todos.
            workspace_root: "/tmp".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .expect("the session row");
        s.put_todos(
            session_id,
            &[
                TodoItem {
                    content: "read the harness".into(),
                    status: TodoStatus::Completed,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
                TodoItem {
                    content: "seat the tool".into(),
                    status: TodoStatus::InProgress,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
                TodoItem {
                    content: "render the pane".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
            ],
        )
        .expect("the todo row");
    }
    let cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let h = opened(&cfg, &parts);

    let todos = h.todo_list();
    assert_eq!(todos.len(), 3, "the plan came back whole");
    assert_eq!(todos[0].content, "read the harness");
    assert_eq!(todos[0].status, TodoStatus::Completed);
    assert_eq!(todos[1].status, TodoStatus::InProgress);
    assert_eq!(todos[2].content, "render the pane");
}

#[test]
fn a_session_that_never_wrote_todos_opens_with_an_empty_board() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-todos-empty");
    let path = dir.path().join("sessions.db");
    let cfg = config(&path, "todos-empty-test");
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let h = opened(&cfg, &parts);
    assert!(
        h.todo_list().is_empty(),
        "no list was ever written; the board says so, not an error"
    );
}

/// A directory that removes itself, because a test that leaks a store per run
/// eventually fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// =====================================================================
// The idle clock: who starts it, and the two paths that did not.
// =====================================================================
//
// **The operator, twice.** First on the feature: *"at which point my todos will be
// reminded to a model?"* — then on the gap, with the evidence in front of them:
// *"so when I bring session back it will not fire right now - this is exactly what i
// see with rano"*.
//
// They were right, and the cause is one line of provenance: `Sessions::rearm_todo_nag`
// is the ONLY writer of `nag_due`, and it had exactly one caller — the end of
// `after_turn`, whose own comment says *"Every turn ends here, so this is where the
// idle clock starts."* So the clock counted from a TURN BOUNDARY and nothing else,
// which left two paths with no clock at all:
//
//   · a row the OPERATOR adds to an IDLE session — the model is quiet, the operator
//     has just said what they want done, and nothing ever reminds it;
//   · a session REOPENED with an unfinished plan — `nag_due` is in-memory, so a
//     daemon that came back had nothing armed and the idle worker had nothing to wake
//     for. This is the rano case exactly.
//
// Both now arm. What is asserted here is that a DEADLINE EXISTS after each, which is
// the whole of the fix: whether it then fires is `deliver_due_nags`' business and is
// the same code the turn-end path has always used.

use letibot_harnessd::Sessions;
use letibot_harnessd::sessions::Outcome;
use letibot_sessionlog::event::{TodoBy as WireTodoBy, TodoEntry, TodoStatus as WireTodoStatus};
use letibot_sessionlog::hub::{CommandKind, QueuedCommand};
use letibot_sessionlog::registry::Registry;

/// A store holding one session with an unfinished plan. The session row first —
/// the todo row carries a foreign key to it.
fn a_session_with_an_open_plan(tag: &str) -> (TempDir, std::path::PathBuf, String) {
    let dir = TempDir::new(tag);
    let path = dir.path().join("sessions.db");
    let session_id = format!("nag-{tag}");
    let s = Store::open(&path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.clone(),
        title: Some("the nag session".into()),
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/tmp".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
    s.put_todos(
        &session_id,
        &[TodoItem {
            content: "finish the migration".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }],
    )
    .expect("the todo row");
    (dir, path, session_id)
}

#[test]
fn a_session_reopened_with_an_open_plan_arms_its_idle_clock() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (_dir, path, session_id) = a_session_with_an_open_plan("rearm-open");
    let cfg = config(&path, &session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    registry
        .create(&session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let sessions = Sessions::open_first(&parts, cfg.clone(), registry).expect("the session opens");

    assert!(
        sessions.next_nag_at().is_some(),
        "**a reopened session with outstanding work has a deadline** — before this, the only \
         arming point was the end of a turn, so a daemon that came back sat for ever with an \
         unfinished plan and nothing to wake the idle worker for. The operator's own case: \
         *\"this is exactly what i see with rano\"*."
    );
}

#[test]
fn a_reopened_session_with_a_finished_plan_arms_nothing() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (_dir, path, session_id) = a_session_with_an_open_plan("rearm-done");
    // Every row completed: a plan with nothing open is not a plan to nag about.
    Store::open(&path)
        .expect("the store")
        .put_todos(
            &session_id,
            &[TodoItem {
                content: "finish the migration".into(),
                status: TodoStatus::Completed,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .expect("the todo row");
    let cfg = config(&path, &session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    registry
        .create(&session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let sessions = Sessions::open_first(&parts, cfg.clone(), registry).expect("the session opens");

    assert!(
        sessions.next_nag_at().is_none(),
        "**and a finished plan still costs no wake** — the arming is `nag_should_arm`'s \
         decision, not a consequence of reopening, so the common case is unchanged"
    );
}

#[test]
fn a_row_the_operator_adds_to_an_idle_session_arms_the_clock() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (_dir, path, session_id) = a_session_with_an_open_plan("rearm-set");
    // Start from a FINISHED plan so the open itself arms nothing, and the only
    // thing that can arm is the operator's own row.
    Store::open(&path)
        .expect("the store")
        .put_todos(&session_id, &[])
        .expect("clearing the plan");
    let cfg = config(&path, &session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    registry
        .create(&session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");
    assert!(
        sessions.next_nag_at().is_none(),
        "nothing to check yet, so nothing is armed"
    );

    // The head pushes a row — the same command `push-operator-todos` puts on the queue
    // when the operator adds one.
    let cmd = QueuedCommand {
        head_id: "test-head".into(),
        identity: "dead".into(),
        client_request_id: "req-1".into(),
        at_seq: 0,
        kind: CommandKind::SetOperatorTodos {
            items: vec![TodoEntry {
                content: "add the migration notes".into(),
                status: WireTodoStatus::Pending,
                by: WireTodoBy::Operator,
                when: None,
                needs: Vec::new(),
            }],
        },
    };
    let outcome = sessions.dispatch(&session_id, &cmd);
    assert!(
        matches!(outcome, Outcome::Ignored),
        "the operator's own list is not a tool call and is not gated"
    );

    assert!(
        sessions.next_nag_at().is_some(),
        "**a row the operator adds to a quiet session starts the clock** — this is the \
         other half of the same gap: before, `SetOperatorTodos` set the board and nothing \
         else, so work the operator had just asked for was never mentioned again"
    );
}
