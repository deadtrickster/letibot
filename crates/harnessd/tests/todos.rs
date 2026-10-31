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
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
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
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
        enqueued_ms: 0,
        kind: CommandKind::SetOperatorTodos {
            items: vec![TodoEntry {
                content: "add the migration notes".into(),
                status: WireTodoStatus::Pending,
                by: WireTodoBy::Operator,
                when: None,
                needs: Vec::new(),
            }],
            moved: Vec::new(),
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

/// **R77: an operator's ADD arms the check at the shorter window, not the idle minute.** What
/// tells an add from a head re-pushing its half is `the_operator_added`, pinned in
/// `sessions/idle_nag.rs`.
///
/// The session opens with the model's row open, so its clock is already armed a minute out. The
/// operator's row is added LAST, the way the head appends it; the deadline that follows is the
/// operator-add window's (ten seconds), which is the daemon half of *"didnt arrive the next nag"*.
#[test]
fn a_row_the_operator_adds_arms_the_check_sooner_than_the_idle_window() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (_dir, path, session_id) = a_session_with_an_open_plan("rearm-add-sooner");
    let cfg = config(&path, &session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    registry
        .create(&session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");
    let push = |content: &str| QueuedCommand {
        head_id: "test-head".into(),
        identity: "dead".into(),
        client_request_id: "req-1".into(),
        at_seq: 0,
        enqueued_ms: 0,
        kind: CommandKind::SetOperatorTodos {
            items: vec![TodoEntry {
                content: content.into(),
                status: WireTodoStatus::Pending,
                by: WireTodoBy::Operator,
                when: None,
                needs: Vec::new(),
            }],
            moved: Vec::new(),
        },
    };

    sessions.dispatch(&session_id, &push("make tier 1 green"));
    let left = sessions
        .next_nag_at()
        .expect("the add arms the check")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        left <= std::time::Duration::from_secs(10),
        "**the operator's add is checked at its own short window**, not a minute out: {left:?}"
    );
}

/// **THE OPERATOR'S VERB REACHES A ROW THE MODEL WROTE — end to end, through the daemon.**
///
/// The hole, MEASURED: a row the model wrote that waits on a person's hand could not be silenced
/// at all — `todo_write` refuses `postponed` (a model that could silence its own plan could
/// abandon it), and `/todo postpone|resume N` numbered the operator's own rows. So the operator's
/// verb now takes the row's own WORDS, and `moved` carries the state move beside the half they
/// own.
///
/// What only this crate can assert is the seam: the words are resolved against the MODEL's half
/// on the daemon's own board, the row is set aside rather than touched in any other way, and the
/// write is PERSISTED — a silence that lived only in memory would be gone at the next restart,
/// which is the one thing `Postponed` was asked for (*"they persist"*).
#[test]
fn the_operators_verb_sets_aside_a_row_the_model_wrote() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (_dir, path, session_id) = a_session_with_an_open_plan("moved-model-row");
    let cfg = config(&path, &session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let mut h = opened(&cfg, &parts);

    // The operator's verb, as the head sends it: their half is empty and the move names the
    // model's row by its words.
    h.set_operator_todos(
        Vec::new(),
        &[("finish the migration".into(), TodoStatus::Postponed)],
    );

    let rows = Store::open(&path)
        .expect("the store")
        .todos(&session_id)
        .expect("the session's rows");
    let row = rows
        .iter()
        .find(|t| t.content == "finish the migration")
        .unwrap_or_else(|| panic!("the model's row is still on the board: {rows:#?}"));
    assert_eq!(
        row.status,
        TodoStatus::Postponed,
        "**the model's row is set aside, and it is still there**: {rows:#?}"
    );
    assert_eq!(row.by, TodoBy::Model, "and it is still the model's row");
    assert_eq!(rows.len(), 1, "nothing was added or removed: {rows:#?}");

    // **And the lift**, the same way — the state is reversible, which is what makes it a silence
    // rather than a quiet way to retire somebody else's row.
    h.set_operator_todos(
        Vec::new(),
        &[("finish the migration".into(), TodoStatus::Pending)],
    );
    let rows = Store::open(&path)
        .expect("the store")
        .todos(&session_id)
        .expect("the session's rows");
    assert_eq!(rows[0].status, TodoStatus::Pending, "{rows:#?}");
}

/// **A plan the store did not take is said on the session's log** — R67 item 15.
///
/// `flush_todos`' failures at the operator's door, the review queue's board and a fired condition
/// used to be `eprintln!` and nothing else, so the head that just moved a row was told nothing
/// when the store refused it. The store is made to refuse by moving its `todo` table away through
/// a second connection; a `/todo` move naming a row that is not there is the second sentence.
#[test]
fn a_plan_the_store_refuses_is_said_on_the_hub() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (_dir, path, session_id) = a_session_with_an_open_plan("refused-write");
    let cfg = config(&path, &session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session must open");

    // **`open_private`**: the refusal has to come from OUTSIDE the harness's connection, and
    // `Store::open` would hand back the very connection `h` writes through.
    let other = Store::open_private(&path).expect("a connection of its own");
    other
        .connection()
        .execute_batch("ALTER TABLE todo RENAME TO todo_away")
        .expect("the table moves away");

    h.set_operator_todos(
        vec![TodoItem {
            content: "write the notes".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }],
        &[("a row nobody wrote".into(), TodoStatus::Postponed)],
    );

    let codes: Vec<String> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            letibot_sessionlog::SessionEvent::Warning { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect();
    assert!(
        codes.iter().any(|c| c == "todos_not_stored"),
        "the store refused the operator's row and the head was not told: {codes:?}"
    );
    assert!(
        codes.iter().any(|c| c == "todo_move_unresolved"),
        "a move naming no row was dropped with a stderr line only: {codes:?}"
    );
}
