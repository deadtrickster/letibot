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
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = g.into();
    }
    cfg
}

fn opened<'a>(cfg: &Config, parts: &'a Parts) -> Harness<'a> {
    let hub = Hub::new(&cfg.session_id);
    Harness::open(parts, cfg.clone(), hub).expect("the session must open")
}

#[test]
fn a_resume_comes_back_with_the_plan_the_model_was_working_from() {
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
                },
                TodoItem {
                    content: "seat the tool".into(),
                    status: TodoStatus::InProgress,
                    by: TodoBy::Model,
                },
                TodoItem {
                    content: "render the pane".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Model,
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
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
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
