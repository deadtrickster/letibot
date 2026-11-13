//! Writes to the session store that a failure must not lose.

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

/// A todo write the store refused is made by the next flush: `flush_todos` recorded the version BEFORE the write, so a
/// failure left the next flush seeing nothing to do and a restart reverted the operator's answers and consumed
/// conditions. The store is made to refuse by moving its `todo` table away, then put back.
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

    // `open_private`: the refusal has to come from OUTSIDE the harness's connection, and
    // `Store::open` would hand back the very connection `h` writes through.
    let other = Store::open_private(&path).expect("a connection of its own");
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

/// A store write past the store's own minute of retries is waited for, not failed on: a write that comes back busy is
/// taken again after the pause and the wait is said on the session's log, while a failure that is not busy is returned
/// at once.
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

/// A person's stop ends the wait on a busy store: the operator pressed esc-esc on a
/// turn that was waiting for another daemon's write lock, and the wait ran to its patience before
/// the stop was read. The session is closing, so the write is not abandoned; a STOP is not.
#[test]
fn a_stop_ends_the_wait_on_a_busy_store() {
    let dir = Scratch::new("r91-10-stop");
    let Some((h, _path)) = harness_on_a_store(&dir, "r91-10-stop") else {
        return;
    };
    // The interrupt an esc-esc puts on the session's own queue, through the hub the head submits to.
    let head = h.hub.attach("tui", "dead", Default::default(), 0);
    h.hub.submit(
        &head.head_id,
        "stop-1",
        0,
        letibot_sessionlog::hub::CommandKind::Interrupt {
            reason: "esc".into(),
        },
    );
    let began = std::time::Instant::now();
    let mut n = 0;
    let r: Result<usize, _> = h.store_patiently("todos", |_| {
        n += 1;
        if n > 1 {
            return Ok(n);
        }
        Err(letibot_tokencore::store::StoreError::Sql(
            letibot_tokencore::rusqlite::Error::SqliteFailure(
                letibot_tokencore::rusqlite::ffi::Error::new(
                    letibot_tokencore::rusqlite::ffi::SQLITE_BUSY,
                ),
                Some("database is locked".into()),
            ),
        ))
    });
    let took = began.elapsed();
    let why = r.expect_err("a stopped write is not made").to_string();
    assert!(
        why.contains("stopped"),
        "the refusal says the stop, not that the session closed: {why}"
    );
    assert!(
        took < std::time::Duration::from_secs(4),
        "the wait ran {took:?}: the stop was not read until the patience ran out"
    );
    assert_eq!(
        n, 1,
        "one attempt: the stop ends the wait, it does not retry it"
    );
}

/// The whole daemon is stalled by one session's wait, so every other session's head is told why
///: the worker runs one turn at a time, and a session left to read `queued` with no
/// reason is the operator's report.
#[test]
fn a_wait_on_the_store_is_said_on_every_other_session_too() {
    let dir = Scratch::new("r91-10-others");
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let path = dir.0.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path.clone());
    cfg.session_id = "s-waiting".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let registry = letibot_sessionlog::registry::Registry::new();
    let me = registry.new_hub("s-waiting".to_string());
    let other = registry.new_hub("s-elsewhere".to_string());
    for (hub, id) in [(&me, "s-waiting"), (&other, "s-elsewhere")] {
        registry
            .adopt(
                hub.clone(),
                id.to_string(),
                letibot_sessionlog::registry::SessionWiring::default(),
                None,
            )
            .expect("the session registers");
    }
    let h = Harness::open_with_registry(&parts, cfg, me, None, None, Arc::clone(&registry))
        .expect("the session opens");
    let busy = || {
        letibot_tokencore::store::StoreError::Sql(
            letibot_tokencore::rusqlite::Error::SqliteFailure(
                letibot_tokencore::rusqlite::ffi::Error::new(
                    letibot_tokencore::rusqlite::ffi::SQLITE_BUSY,
                ),
                Some("database is locked".into()),
            ),
        )
    };
    let mut n = 0;
    h.store_patiently("todos", |_| {
        n += 1;
        if n < 2 { Err(busy()) } else { Ok(n) }
    })
    .expect("a busy store is waited out");
    let codes = |hub: &Arc<Hub>| -> Vec<String> {
        hub.retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .filter(|(c, _)| c.starts_with("store_busy"))
            .map(|(c, d)| format!("{c}: {d}"))
            .collect()
    };
    let mine = codes(&h.hub);
    assert!(
        mine.iter().any(|c| c.starts_with("store_busy_wait")),
        "the waiting session says so: {mine:?}"
    );
    let theirs = codes(&other);
    assert_eq!(
        theirs.len(),
        1,
        "the other session was told once, and it is not left reading `queued`: {theirs:?}"
    );
    assert!(
        theirs[0].starts_with("store_busy_queue")
            && theirs[0].contains("s-waiting")
            && theirs[0].contains("one turn at a time"),
        "the other session's sentence names the one that is waiting: {}",
        theirs[0]
    );
    registry.close();
}

/// A `/mode` taken by a running turn is written down: the running turn's door applied the point and wrote nothing,
/// trusting the worker's door, which never saw the command. Read back the way a restart reads it, from a fresh store.
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

/// A settlement a door took for a turn whose row the store refused is still owed: the
/// door must not mark it delivered before it knows its row went in, or a refusal loses it.
#[test]
fn a_settlement_a_refused_door_never_said_is_still_owed() {
    use crate::jobwatch::{BackgroundKind, JobCompletion};
    let dir = Scratch::new("r91-11-door");
    let Some((mut h, path)) = harness_on_a_store(&dir, "r91-11-door") else {
        return;
    };
    let watch = h.job_watch.clone().expect("a session's watcher set");
    watch.queue_for_test(JobCompletion {
        memory: None,
        kind: BackgroundKind::Job,
        owner: "r91-11-door".into(),
        job: "j1".into(),
        command: "cargo test".into(),
        slug: None,
        state: "exited 0".into(),
        produced: 12,
        elapsed_ms: 1500,
        detail: String::new(),
        arrived: std::time::SystemTime::now(),
        later: false,
    });

    // The row the door writes has nowhere to go, and not for a busy store's reason, so the door
    // answers at once rather than waiting the store out.
    let other = Store::open_private(&path).expect("a connection of its own");
    other
        .connection()
        .execute_batch("ALTER TABLE transcript_item RENAME TO transcript_item_away")
        .expect("the table moves away");
    assert!(
        h.run_after_reseat("the prompt moved").is_err(),
        "the store refused the door's own row"
    );
    assert_eq!(
        watch.held_count(),
        1,
        "the settlement was taken and marked delivered for a turn that never opened"
    );

    // And the store takes rows again: the next door carries it, which is how the session gets it.
    other
        .connection()
        .execute_batch("ALTER TABLE transcript_item_away RENAME TO transcript_item")
        .expect("the table comes back");
    assert_eq!(watch.held_count(), 1, "still owed until a turn carries it");
}

/// A settlement a turn DID say is delivered however that turn ended: the door is judged by whether
/// its row went in, and a turn that opened and then failed has already put the words in front of the
/// model. Judged by the turn's success instead, the same batch is held again and said twice.
#[test]
fn a_batch_a_turn_said_and_then_failed_is_not_owed_again() {
    use crate::jobwatch::{BackgroundKind, JobCompletion};
    let dir = Scratch::new("r91-11-said");
    let Some((mut h, _path)) = harness_on_a_store(&dir, "r91-11-said") else {
        return;
    };
    let watch = h.job_watch.clone().expect("a session's watcher set");
    // Held, as a settlement that landed while the last turn was working: the batch this turn rides.
    watch.hold(vec![JobCompletion {
        memory: None,
        kind: BackgroundKind::Job,
        owner: "r91-11-said".into(),
        job: "j2".into(),
        command: "cargo build".into(),
        slug: None,
        state: "exited 0".into(),
        produced: 12,
        elapsed_ms: 1500,
        detail: String::new(),
        arrived: std::time::SystemTime::now(),
        later: false,
    }]);
    // The row goes in and the rounds then fail: this box has no model, so the round after the
    // appends answers with an error rather than with words.
    assert!(
        h.submit("carry on").is_err(),
        "the premise: the turn opened and its rounds failed"
    );
    assert_eq!(
        watch.held_count(),
        0,
        "the model was handed the batch by a row that went in; a failed round does not un-say it"
    );
    let said = h
        .session
        .items
        .iter()
        .filter(|i| format!("{i:?}").contains("a job you backgrounded has ended"))
        .count();
    assert_eq!(
        said, 1,
        "and it is said once, in the one row that carried it"
    );
}

/// A job settlement as a held batch, for the doors that carry one into a turn of the harness's own.
fn held_job(owner: &str) -> crate::jobwatch::JobCompletion {
    crate::jobwatch::JobCompletion {
        memory: None,
        kind: crate::jobwatch::BackgroundKind::Job,
        owner: owner.into(),
        job: "j2".into(),
        command: "cargo build".into(),
        slug: None,
        state: "exited 0".into(),
        produced: 12,
        elapsed_ms: 1500,
        detail: String::new(),
        arrived: std::time::SystemTime::now(),
        later: false,
    }
}

/// The wall continuation and the re-seat turn are doors too: the row carrying the batch went in,
/// so a round that fails afterwards does not make the batch owed again.
#[test]
fn a_batch_a_wall_door_said_and_then_failed_is_not_owed_again() {
    let dir = Scratch::new("hdfix-wall-said");
    let Some((mut h, _path)) = harness_on_a_store(&dir, "hdfix-wall-said") else {
        return;
    };
    let watch = h.job_watch.clone().expect("a session's watcher set");
    watch.hold(vec![held_job("hdfix-wall-said")]);
    assert!(
        h.continue_after_wall().is_err(),
        "the premise: the rows went in and the rounds failed"
    );
    assert_eq!(
        watch.held_count(),
        0,
        "continue_after_wall put a said batch back"
    );

    watch.hold(vec![held_job("hdfix-wall-said")]);
    assert!(h.run_after_reseat("re-seated").is_err());
    assert_eq!(
        watch.held_count(),
        0,
        "run_after_reseat put a said batch back"
    );
}

/// Opening a session by id is the way back from an archive: the row is cleared at the one door every
/// opening takes, and the note names the reason it had been away. No harness and no GGUF - the seam
/// is the store write the open makes.
#[test]
fn opening_an_archived_session_takes_it_back_into_the_list() {
    let dir = Scratch::new("unarchive-on-open");
    let store = Store::open(&dir.0.join("one.db")).expect("a store");
    store
        .put_session(&SessionRecord {
            id: "s-away".into(),
            title: Some("the one put away".into()),
            model_id: "qwen".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            approvers: Vec::new(),
            role: None,
            parent_session_id: None,
        })
        .expect("a session row");
    store
        .set_archived("s-away", true, Some("the question was answered"))
        .expect("the archive");
    assert!(store.list_sessions().expect("the list").is_empty());

    let stored = store.session("s-away").expect("a read").expect("the row");
    let mut notes = Vec::new();
    crate::harness::open::unarchive_on_open(Some(&store), &stored, &mut notes);

    assert_eq!(
        store.list_sessions().expect("the list").len(),
        1,
        "opening it by id put it back"
    );
    assert_eq!(notes.len(), 1, "and the head is told why it was away");
    assert!(
        notes[0].contains("the question was answered"),
        "the note carries the archiver's reason: {notes:?}"
    );
}

/// A head's two halves of the list are one store read each: what a daemon is not holding is in
/// `list`, and what it has put out of sight is in `list_archived` - the group a head draws - and a
/// row is in exactly one of them.
#[test]
fn the_archived_half_of_the_list_is_read_from_the_store() {
    use letibot_sessionlog::registry::SessionSource;

    let dir = Scratch::new("archived-half");
    let path = dir.0.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.store = Some(path.clone());
    let open = Store::open(&path).expect("a store");
    for (id, title) in [
        ("s-here", "the one in the list"),
        ("s-away", "the one away"),
    ] {
        open.put_session(&SessionRecord {
            id: id.into(),
            title: Some(title.into()),
            model_id: "qwen".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            approvers: Vec::new(),
            role: None,
            parent_session_id: None,
        })
        .expect("a session row");
    }
    open.set_archived("s-away", true, Some("the question was answered"))
        .expect("the archive");
    drop(open);

    // A daemon opening onto that store: the two halves it would answer a head with.
    let sessions = crate::sessions::StoreSessions::open(&cfg).expect("the daemon's store");
    let listed: Vec<String> = SessionSource::list(&*sessions)
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    assert_eq!(
        listed,
        vec!["s-here"],
        "the list leaves the archived one out"
    );
    let archived = SessionSource::list_archived(&*sessions);
    assert_eq!(
        archived
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["s-away"],
        "and the archived half holds it"
    );
    assert_eq!(archived[0].title, "the one away", "with the row's own name");
}
