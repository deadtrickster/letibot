//! **Writes to the session store that a failure must not lose** (R91.6, R91.7).

use super::*;
use letibot_tokencore::store::{TodoBy, TodoStatus};

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A harness over a fresh store, or `None` when this box has no vocabulary GGUF.
fn harness_on_a_store(dir: &Scratch, id: &str) -> Option<(Harness, std::path::PathBuf)> {
    let gguf = crate::vocab_gguf()?;
    let path = dir.0.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path.clone());
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
    Some((h, path))
}

/// **A todo write the store refused is made by the next flush** (R91.7).
///
/// `flush_todos` recorded the board's version BEFORE `put_todos`, so a write that failed left
/// the next flush seeing nothing to do: the store kept the old board until the board moved
/// again, and a restart reverted the operator's answers and consumed conditions. The store is
/// made to refuse by moving its `todo` table away through a second connection, then put back.
#[test]
fn a_todo_write_the_store_refused_is_made_by_the_next_flush() {
    let dir = Scratch::new("r91-7-todos");
    let Some((mut h, path)) = harness_on_a_store(&dir, "r91-7-todos") else {
        return;
    };
    h.todos.set_operator(vec![TodoItem {
        content: "write the notes".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }]);

    let other = Store::open(&path).expect("a second connection");
    other
        .connection()
        .execute_batch("ALTER TABLE todo RENAME TO todo_away")
        .expect("the table moves away");
    assert!(h.flush_todos().is_err(), "the store refused the board");
    other
        .connection()
        .execute_batch("ALTER TABLE todo_away RENAME TO todo")
        .expect("the table comes back");

    // The board has not moved since; the store still has to be given it.
    h.flush_todos().expect("the store takes it now");
    let rows = other.todos("r91-7-todos").expect("the session's rows");
    assert!(
        rows.iter().any(|r| r.content == "write the notes"),
        "the refused write was never made again, so a restart reverts the board: {rows:#?}"
    );
}

/// **A store write past the store's own minute of retries is waited for, not failed on**
/// (R91.6) — the patience `persist_reporting` has, now shared by the todo flush. A write that
/// comes back busy is taken again after the pause, and the wait is said on the session's log;
/// a failure that is not busy is returned at once.
#[test]
fn a_busy_store_write_is_waited_out_and_said() {
    let dir = Scratch::new("r91-6-patience");
    let Some((h, _path)) = harness_on_a_store(&dir, "r91-6-patience") else {
        return;
    };
    fn busy() -> letibot_tokencore::store::StoreError {
        letibot_tokencore::store::StoreError::Sql(
            letibot_tokencore::rusqlite::Error::SqliteFailure(
                letibot_tokencore::rusqlite::ffi::Error::new(
                    letibot_tokencore::rusqlite::ffi::SQLITE_BUSY,
                ),
                Some("database is locked".into()),
            ),
        )
    }
    let mut n = 0;
    let got = h
        .store_patiently("todos", |_| {
            n += 1;
            if n < 2 { Err(busy()) } else { Ok(n) }
        })
        .expect("a busy store is waited out");
    assert_eq!(got, 2);
    let said = h.hub.retained().iter().any(
        |e| matches!(&e.event, SessionEvent::Warning { code, .. } if code == "store_busy_wait"),
    );
    assert!(said, "the wait was not said on the session's log");

    let mut n = 0;
    let r: Result<(), _> = h.store_patiently("todos", |_| {
        n += 1;
        Err(letibot_tokencore::store::StoreError::NotFound("x".into()))
    });
    assert!(r.is_err());
    assert_eq!(n, 1, "a failure that is not busy is not waited for");
}

/// **A `/mode` taken by a running turn is written down** — the operator, twice: *"i restart stroppy
/// and allow-all goes"*. The running turn's door (`apply_queued_mode`) applied the point and wrote
/// nothing, trusting the worker's door, which never saw the command. Read back the way a restart
/// reads it: a fresh store, the session's own row and the project's.
#[test]
fn a_mode_set_mid_turn_survives_a_restart() {
    let dir = Scratch::new("mode-midturn");
    let Some((mut h, path)) = harness_on_a_store(&dir, "s-midturn") else {
        return;
    };
    let head = h.hub.attach("tui", "dead", Default::default(), 0);
    h.hub.submit(
        &head.head_id,
        "m1",
        0,
        letibot_sessionlog::hub::CommandKind::Mode {
            name: "writes-allowed".into(),
            consented: false,
        },
    );
    h.apply_queued_mode();
    assert_eq!(h.config().mode.name, "writes allowed", "the session moved");
    let fresh = crate::modes::ModeStore::load(&path);
    assert_eq!(
        fresh.session("s-midturn").map(|(m, _)| m.name),
        Some("writes allowed"),
        "the session's own row is there for the next open"
    );
    assert_eq!(
        fresh.for_project(h.workspace()).name,
        "writes allowed",
        "and the project's"
    );
}
