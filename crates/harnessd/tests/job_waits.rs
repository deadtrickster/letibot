//! A todo row waiting on a job that no longer runs is freed - at open, and at the nag clock's
//! tick when no job-end event reached it - with one note saying how the job ended. A freed row
//! that can start gets that note in a turn, never as a turn-less row.

use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::Outcome;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::hub::{CommandKind, QueuedCommand};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{
    JobRecord, SessionRecord, Store, TodoBy, TodoItem, TodoNeed, TodoStatus,
};
use letibot_turn::Endpoint;

mod support;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // Never this box's model server: a turn that ran would be the defect.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

fn job(handle: &str, state: &str) -> JobRecord {
    JobRecord {
        handle: handle.into(),
        command: "cargo test".into(),
        how: "you asked for this to run in the background".into(),
        state: state.into(),
        produced: 0,
        elapsed_ms: 1000,
        redirect: None,
    }
}

fn row(content: &str, jobs: &[&str]) -> TodoItem {
    TodoItem {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: jobs
            .iter()
            .map(|h| TodoNeed::Job {
                handle: (*h).into(),
            })
            .collect(),
    }
}

fn seeded(tag: &str, todos: &[TodoItem], jobs: &[JobRecord]) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "harnessd-job-waits-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let session_id = format!("job-waits-{tag}");
    let s = Store::open(&path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.clone(),
        title: Some("waits on jobs".into()),
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/tmp".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
    s.put_todos(&session_id, todos).expect("the todo rows");
    for j in jobs {
        s.put_job(&session_id, j).expect("the job row");
    }
    (path, session_id)
}

fn opened<'a>(
    parts: &'a Parts,
    path: &std::path::Path,
    session_id: &str,
) -> (Sessions<'a>, std::sync::Arc<Registry>) {
    let cfg = config(path, session_id);
    let registry = Registry::new();
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let sessions =
        Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("the session opens");
    (sessions, registry)
}

fn notes(sessions: &Sessions, id: &str) -> String {
    sessions
        .harness_of(id)
        .expect("open")
        .items()
        .iter()
        .map(letibot_sessionlog::view::body_of)
        .filter(|b| b.starts_with("[todo] "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn job_needs(sessions: &Sessions, id: &str) -> Vec<(String, Vec<TodoNeed>)> {
    sessions
        .harness_of(id)
        .expect("open")
        .todo_list()
        .into_iter()
        .map(|t| (t.content, t.needs))
        .collect()
}

/// A restore whose rows wait on jobs that ended or that the store no longer holds frees them, and
/// a failed job's exit status is in the note. The rows still wait on an open row, so nothing can
/// start and the note is a row of its own.
#[test]
fn a_restore_resolves_stale_waits_on_ended_and_missing_jobs() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let after_prep = |content: &str, jobs: &[&str]| {
        let mut r = row(content, jobs);
        r.needs.push(TodoNeed::Row {
            content: "prep".into(),
        });
        r
    };
    let (path, id) = seeded(
        "restore",
        &[
            row("prep", &[]),
            after_prep("K4 surface family", &["j86131", "j86141"]),
            after_prep("K6 land the records", &["j86141", "j99999", "j86254"]),
        ],
        &[
            job("j86131", "exited 0"),
            job("j86141", "exited 0"),
            job("j86254", "exited 101"),
        ],
    );
    let parts = Parts::load(&config(&path, &id)).expect("the vocabulary must load");
    let (sessions, _registry) = opened(&parts, &path, &id);

    for (content, needs) in job_needs(&sessions, &id) {
        assert!(
            needs.iter().all(|n| !matches!(n, TodoNeed::Job { .. })),
            "`{content}` still waits on {needs:?}"
        );
    }
    let said = notes(&sessions, &id);
    assert!(said.contains("j86131 ended (exit 0) at "), "{said}");
    assert!(
        said.lines()
            .any(|l| l.starts_with("[todo] j86141 ended (exit 0) at ")
                && l.ends_with(
                    "- no longer a wait of `K4 surface family`, `K6 land the records`."
                )),
        "one line per job, naming every row it freed: {said}"
    );
    assert!(
        said.contains("j86254 ended (exit 101, failed) at "),
        "{said}"
    );
    assert!(
        said.contains("j99999 is not in this session's job table"),
        "{said}"
    );
    // Persisted, so the next daemon does not resolve them a second time.
    let stored = Store::open(&path).unwrap().todos(&id).unwrap();
    assert!(
        stored
            .iter()
            .all(|t| t.needs.iter().all(|n| !matches!(n, TodoNeed::Job { .. }))),
        "{stored:?}"
    );
}

/// Whether the worker has a wake waiting for this session.
fn woken(registry: &Registry, id: &str) -> bool {
    matches!(
        registry.next_work_until(Some(std::time::Instant::now())),
        letibot_sessionlog::registry::WorkOrIdle::Work(
            letibot_sessionlog::registry::Work::Woken(w)
        ) if w == id
    )
}

/// Every transcript row's body that carries `needle`.
fn rows_saying(sessions: &Sessions, id: &str, needle: &str) -> Vec<String> {
    sessions
        .harness_of(id)
        .expect("open")
        .items()
        .iter()
        .map(letibot_sessionlog::view::body_of)
        .filter(|b| b.contains(needle))
        .collect()
}

/// A job that ended with no job-end event delivered - here, one the operator's row names after it
/// was already over - is resolved at the nag clock's next tick. The freed row can start, so the
/// note is no turn-less row: the session is woken and the note opens that turn.
#[test]
fn the_nag_tick_resolves_a_wait_no_job_end_reached() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (path, id) = seeded("tick", &[], &[job("j7", "exited 0")]);
    let parts = Parts::load(&config(&path, &id)).expect("the vocabulary must load");
    let (mut sessions, registry) = opened(&parts, &path, &id);
    // The wake a restore rings at open, so the one asserted below is the sweep's.
    while woken(&registry, &id) {}
    let out = sessions.dispatch(
        &id,
        &QueuedCommand {
            head_id: "test-head".into(),
            identity: "dead".into(),
            client_request_id: "req-1".into(),
            at_seq: 0,
            enqueued_ms: 0,
            kind: CommandKind::SetOperatorTodos {
                items: vec![letibot_sessionlog::event::TodoEntry {
                    content: "after the gate".into(),
                    status: letibot_sessionlog::event::TodoStatus::Pending,
                    by: letibot_sessionlog::event::TodoBy::Operator,
                    when: None,
                    needs: vec![letibot_sessionlog::event::TodoNeed::Job {
                        handle: "j7".into(),
                    }],
                }],
                moved: Vec::new(),
            },
        },
    );
    assert!(matches!(out, Outcome::Ignored));
    assert_eq!(
        job_needs(&sessions, &id),
        vec![(
            "after the gate".to_string(),
            vec![TodoNeed::Job {
                handle: "j7".into()
            }]
        )],
        "the premise: the wait is on the board until the tick"
    );

    sessions.deliver_due_nags();

    assert_eq!(
        job_needs(&sessions, &id),
        vec![("after the gate".to_string(), Vec::new())]
    );
    assert_eq!(notes(&sessions, &id), "", "no turn-less row");
    assert!(
        woken(&registry, &id),
        "the freed row is ready: a turn is rung"
    );
    // One-time: a second tick has nothing to say and rings nothing.
    sessions.deliver_due_nags();
    assert!(!woken(&registry, &id));

    // The wake's turn opens with the note. Its model is unreachable; the row is in all the same.
    let _ = sessions.wake(&id);
    let said = rows_saying(&sessions, &id, "[todo] j7 ended (exit 0) at ");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("- no longer a wait of `after the gate`."),
        "{said:?}"
    );
}

/// A monitor or pid wait the last daemon left on the board is over once nothing watches or runs
/// it here. Open takes it off with a note, as for a job, so the pane does not draw it as a wait.
#[test]
fn a_monitor_wait_is_resolved_at_open_after_restart() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let waiting = |content: &str, need: TodoNeed| TodoItem {
        needs: vec![
            need,
            TodoNeed::Row {
                content: "prep".into(),
            },
        ],
        ..row(content, &[])
    };
    let (path, id) = seeded(
        "monitor",
        &[
            row("prep", &[]),
            waiting(
                "after the watch",
                TodoNeed::Monitor {
                    name: "ci-watch".into(),
                },
            ),
            // pid 1 runs, but never with this start time: the process waited on is gone.
            waiting("after the build", TodoNeed::Pid { pid: 1, start: 1 }),
        ],
        &[],
    );
    let parts = Parts::load(&config(&path, &id)).expect("the vocabulary must load");
    let (sessions, _registry) = opened(&parts, &path, &id);

    for (content, needs) in job_needs(&sessions, &id) {
        assert!(
            needs.iter().all(|n| matches!(n, TodoNeed::Row { .. })),
            "`{content}` still waits on {needs:?}"
        );
    }
    let said = notes(&sessions, &id);
    assert!(
        said.contains(
            "[todo] monitor ci-watch is no longer watching (it fired, expired or was retired) - \
             no longer a wait of `after the watch`."
        ),
        "{said}"
    );
    assert!(
        said.contains("[todo] pid 1 is no longer running - no longer a wait of `after the build`."),
        "{said}"
    );
    let stored = Store::open(&path).unwrap().todos(&id).unwrap();
    assert!(
        stored
            .iter()
            .all(|t| t.needs.iter().all(|n| matches!(n, TodoNeed::Row { .. }))),
        "{stored:?}"
    );
}
