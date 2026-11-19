//! The operator's `/nag pause`, end to end through the daemon's own door - the verb as a
//! head sends it (`CommandKind::Slash`), the clock it stands down, the hub rows that make
//! it loud, the settings row a head draws it from, and the session row that carries it

use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::Outcome;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::hub::{CommandKind, Hub, QueuedCommand};
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::{NagPause, SessionEvent};
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoItem, TodoStatus};
use letibot_turn::Endpoint;

mod support;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // Never this box's model server, and no ladder: a turn that ran would be the defect, and it
    // should fail at once rather than hide behind doubling waits.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

/// A store holding one session with an unfinished plan - `todos.rs`'s fixture.
fn seeded(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "harnessd-nag-pause-{tag}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let session_id = format!("nag-pause-{tag}");
    let s = Store::open(&path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.clone(),
        title: Some("the paused session".into()),
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

fn load_parts(path: &std::path::Path, session_id: &str) -> Parts {
    Parts::load(&config(path, session_id)).expect("the vocabulary must load")
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

fn command(kind: CommandKind) -> QueuedCommand {
    QueuedCommand {
        head_id: "test-head".into(),
        identity: "dead".into(),
        client_request_id: "req-1".into(),
        at_seq: 0,
        kind,
        enqueued_ms: 0,
    }
}

fn slash(sessions: &mut Sessions, session_id: &str, line: &str) {
    let out = sessions.dispatch(
        session_id,
        &command(CommandKind::Slash { line: line.into() }),
    );
    assert!(
        matches!(out, Outcome::Ignored),
        "`/{line}` is answered, not refused"
    );
}

/// The warning rows on the hub with this code, as their text.
fn rows(hub: &Hub, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

/// Whether a `[todo check]` turn was started on this hub - the self-addressed prompt a delivered
/// check IS (`Speaker::Agent`).
fn checked(hub: &Hub) -> bool {
    hub.retained().iter().any(|e| {
        matches!(
            &e.event,
            SessionEvent::TranscriptContent { item, .. } | SessionEvent::TranscriptAppended { item: Some(item), .. }
                if matches!(
                    &**item,
                    letibot_transcript::TranscriptItem::User {
                        speaker: letibot_transcript::Speaker::Agent,
                        ..
                    }
                )
        )
    })
}

fn nags_row(registry: &Registry, session_id: &str) -> String {
    registry
        .settings(session_id)
        .into_iter()
        .find(|r| r.key == "nags")
        .map(|r| r.value)
        .unwrap_or_else(|| panic!("the settings carry a `nags` row"))
}

/// `/nag pause` stops the arming AND the delivery - the clock stands down, a delivery pass whose
/// window has long passed runs no turn, and an operator's added row arms nothing either. The hub
/// says so, and so does the settings row.
#[test]
fn a_paused_session_is_neither_armed_nor_checked() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, path, id) = seeded("stops");
    let parts = load_parts(&path, &id);
    let (mut sessions, registry) = opened(&parts, &path, &id);
    // The window, shortened so the open plan would come due at once.
    sessions.set_nag_window(Duration::from_millis(1));
    assert!(
        sessions.next_nag_at().is_some(),
        "the premise: an open plan arms the check"
    );
    assert_eq!(nags_row(&registry, &id), "on");

    slash(&mut sessions, &id, "nag pause");
    assert!(
        sessions.next_nag_at().is_none(),
        "**a paused session arms nothing** — the deadline the open armed is gone with the pause"
    );

    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(sessions.deliver_due_nags(), 0, "no check is delivered");
    let hub = registry.get(&id).expect("the session's hub");
    assert!(!checked(&hub), "no `[todo check]` turn ran");

    // The operator's own row still lands on the board - and earns no check while paused.
    let out = sessions.dispatch(
        &id,
        &command(CommandKind::SetOperatorTodos {
            items: vec![letibot_sessionlog::event::TodoEntry {
                content: "and write it down".into(),
                status: letibot_sessionlog::event::TodoStatus::Pending,
                by: letibot_sessionlog::event::TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            }],
            moved: Vec::new(),
        }),
    );
    assert!(matches!(out, Outcome::Ignored));
    assert!(
        sessions
            .harness_of(&id)
            .expect("open")
            .todo_list()
            .iter()
            .any(|t| t.content == "and write it down"),
        "the board takes the operator's edit"
    );
    assert!(
        sessions.next_nag_at().is_none(),
        "R77's sooner check does not slip past the pause"
    );

    // Loud: the hub row, the verb's own answer, and the settings row.
    assert_eq!(rows(&hub, "nags_paused").len(), 1, "one `nags_paused` row");
    assert!(
        rows(&hub, "slash")
            .iter()
            .any(|r| r.starts_with("/nag pause\n") && r.contains("PAUSED")),
        "the verb answers on the log: {:?}",
        rows(&hub, "slash")
    );
    assert!(
        nags_row(&registry, &id)
            .starts_with("paused until you resume them (by the operator, since "),
        "{}",
        nags_row(&registry, &id)
    );

    // And `/nag resume` puts the clock back.
    slash(&mut sessions, &id, "nag resume");
    assert!(
        sessions.next_nag_at().is_some(),
        "a resumed session is armed again"
    );
    assert_eq!(rows(&hub, "nags_resumed").len(), 1);
    assert_eq!(nags_row(&registry, &id), "on");

    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `/nag pause 1s` turns itself back on - the clock's one deadline is the pause's end, and the
/// delivery pass that reaches it lifts the pause, says so, re-arms the check, and runs no turn.
#[test]
fn a_pause_with_an_end_lifts_itself_and_re_arms() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, path, id) = seeded("ends");
    let parts = load_parts(&path, &id);
    let (mut sessions, registry) = opened(&parts, &path, &id);

    slash(&mut sessions, &id, "nag pause 1s");
    let end = sessions
        .next_nag_at()
        .expect("a pause with an end arms that end");
    let left = end.saturating_duration_since(std::time::Instant::now());
    assert!(
        left <= Duration::from_secs(1),
        "the one deadline is the pause's end, a second out: {left:?}"
    );
    assert!(
        sessions
            .harness_of(&id)
            .expect("open")
            .nag_pause()
            .is_some(),
        "the session is paused"
    );

    std::thread::sleep(left + Duration::from_millis(50));
    assert_eq!(
        sessions.deliver_due_nags(),
        0,
        "the pause's end is not a check — it ran no turn"
    );
    let hub = registry.get(&id).expect("the session's hub");
    assert!(
        !checked(&hub),
        "no `[todo check]` turn ran at the pause's end"
    );
    assert!(
        sessions
            .harness_of(&id)
            .expect("open")
            .nag_pause()
            .is_none(),
        "the pause is over"
    );
    let resumed = rows(&hub, "nags_resumed");
    assert!(
        resumed.len() == 1 && resumed[0].contains("ran out"),
        "the end is said on the hub: {resumed:?}"
    );
    let next = sessions
        .next_nag_at()
        .expect("**and the check is armed again** — the window from now, not never");
    assert!(
        next > end,
        "armed from the resume, not left on the old deadline"
    );
    assert_eq!(
        Store::open(&path)
            .expect("the store")
            .session(&id)
            .expect("read")
            .expect("the row")
            .nags_paused_until,
        None,
        "the row is cleared with it"
    );

    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A pause lasts one daemon's life. Reopened by the same daemon it holds, with the settings row
/// naming who paused it. One a earlier daemon wrote is dropped at open: nags on, the column
/// cleared, and one row saying since when and by whom.
#[test]
fn a_pause_holds_across_a_reopen_and_never_across_a_restart() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, path, id) = seeded("restart");
    let parts = load_parts(&path, &id);
    {
        let (mut sessions, registry) = opened(&parts, &path, &id);
        slash(&mut sessions, &id, "nag pause 2h");
        registry.close();
    }
    let stored = Store::open(&path)
        .expect("the store")
        .nag_pause(&id)
        .expect("read")
        .expect("the pause is on the row");
    assert!(stored.until > 0, "{stored:?}");
    assert_eq!(stored.by.as_deref(), Some("operator"));

    // The same daemon, the session opened again.
    {
        let (sessions, registry) = opened(&parts, &path, &id);
        assert!(
            matches!(
                sessions.harness_of(&id).expect("open").nag_pause(),
                Some(NagPause::Until { .. })
            ),
            "a reopen in the same daemon keeps the pause"
        );
        let end = sessions
            .next_nag_at()
            .expect("the pause's end is armed")
            .saturating_duration_since(std::time::Instant::now());
        assert!(end > Duration::from_secs(3600), "{end:?}");
        let row = nags_row(&registry, &id);
        assert!(
            row.starts_with("paused until ") && row.contains("(by the operator, since "),
            "the settings row names the end and the author: {row}"
        );
        let hub = registry.get(&id).expect("the session's hub");
        assert!(rows(&hub, "nags_resumed").is_empty(), "nothing was dropped");
        registry.close();
    }

    // A pause an earlier daemon wrote, by the model, an hour ago.
    let now = letibot_sessionlog::registry::now_ms() as i64;
    Store::open(&path)
        .expect("the store")
        .set_nag_pause(
            &id,
            Some(&letibot_tokencore::store::StoredNagPause {
                until: 0,
                at_ms: Some(now - 3_600_000),
                by: Some("model".into()),
            }),
        )
        .expect("an earlier daemon's pause");
    {
        let (sessions, registry) = opened(&parts, &path, &id);
        assert!(
            sessions
                .harness_of(&id)
                .expect("open")
                .nag_pause()
                .is_none(),
            "a pause never survives a restart"
        );
        let hub = registry.get(&id).expect("the session's hub");
        let said = rows(&hub, "nags_resumed");
        assert_eq!(said.len(), 1, "one row: {said:?}");
        assert!(
            said[0].starts_with("plan-checks were paused before the restart (since ")
                && said[0].contains(", by the model); they are on again - `/nag pause` to pause"),
            "{}",
            said[0]
        );
        assert_eq!(nags_row(&registry, &id), "on");
        assert!(sessions.next_nag_at().is_some(), "the check is armed again");
        assert_eq!(
            Store::open(&path)
                .expect("the store")
                .nag_pause(&id)
                .expect("read"),
            None,
            "the column is cleared"
        );
        registry.close();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `/nag`, `/prune` or `/clear` from a head, on a session served by its own thread, is run
/// once by that thread. Handing it back to the thread that already owns it loops for ever.
#[test]
fn a_slash_on_its_own_thread_runs_once_and_is_not_handed_on() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, path, id) = seeded("own-thread");
    let parts: &'static Parts = Box::leak(Box::new(load_parts(&path, &id)));
    let (mut sessions, registry) = opened(parts, &path, &id);

    type Seen = std::sync::Mutex<Vec<(String, bool)>>;
    let seen: std::sync::Arc<Seen> = std::sync::Arc::default();
    let sink = seen.clone();
    sessions.serve_each_on_its_own_thread(std::sync::Arc::new(move |_, said| {
        if let letibot_harnessd::sessions::OutcomeSaid::Command(cmd, outcome) = said
            && let CommandKind::Slash { line } = &cmd.kind
        {
            let handed = matches!(outcome, Outcome::HandedOn);
            sink.lock().unwrap().push((line.clone(), handed));
        }
    }));
    let hub = registry.get(&id).expect("the session's hub");
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while hub.owner() != letibot_sessionlog::hub::Owner::OwnThread {
        assert!(
            std::time::Instant::now() < until,
            "the session never got its own thread"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let head = hub.attach(
        "tui",
        "dead",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );

    for (n, line) in ["nag pause", "prune", "clear"].into_iter().enumerate() {
        let seq = hub.retained().last().map(|e| e.seq).unwrap_or(0);
        let _ = hub.submit(
            &head.head_id,
            format!("r-{n}"),
            seq,
            CommandKind::Slash { line: line.into() },
        );
        let of = |handed: bool| {
            seen.lock()
                .unwrap()
                .iter()
                .filter(|(l, h)| l == line && *h == handed)
                .count()
        };
        // Each run is a store call, and under IO load one takes tens of seconds: this waits for the
        // outcome to be recorded, and the deadline is only the one that says it never happened.
        let until = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if of(false) >= 1 || of(true) >= 100 {
                break;
            }
            assert!(
                std::time::Instant::now() < until,
                "`/{line}` never ran on its own thread"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            (of(false), of(true)),
            (1, 0),
            "`/{line}` is run once on its own thread and never handed on"
        );
    }
    registry.close();
    drop(sessions);
    let _ = std::fs::remove_dir_all(dir);
}

/// The `nags_paused` / `nags_resumed` rows name who did it: the model's own `nag_pause_self`
/// queues `/nag` as `agent:<session>`, the operator's head as their name. The settings row is
/// an on/off choice, so the config pane can flip it, and the state follows the verbs it sends.
#[test]
fn the_pause_rows_name_their_author_and_the_row_toggles() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, path, id) = seeded("author");
    let parts = load_parts(&path, &id);
    let (mut sessions, registry) = opened(&parts, &path, &id);
    let row = registry
        .settings(&id)
        .into_iter()
        .find(|r| r.key == "nags")
        .expect("a nags row");
    assert_eq!(row.choices, vec!["on".to_string(), "off".to_string()]);
    assert_eq!(row.value, "on");

    let mut by_model = command(CommandKind::Slash {
        line: "nag pause".into(),
    });
    by_model.identity = format!("agent:{id}");
    assert!(matches!(
        sessions.dispatch(&id, &by_model),
        Outcome::Ignored
    ));
    let hub = registry.get(&id).expect("the session's hub");
    let said = rows(&hub, "nags_paused");
    assert!(said[0].starts_with("nags PAUSED by the model "), "{said:?}");
    assert!(
        nags_row(&registry, &id).contains("(by the model, since "),
        "{}",
        nags_row(&registry, &id)
    );

    slash(&mut sessions, &id, "nag resume");
    let said = rows(&hub, "nags_resumed");
    assert!(
        said[0].starts_with("nags RESUMED by the operator"),
        "{said:?}"
    );
    assert_eq!(nags_row(&registry, &id), "on");

    slash(&mut sessions, &id, "nag pause");
    let said = rows(&hub, "nags_paused");
    assert!(
        said[1].starts_with("nags PAUSED by the operator "),
        "{said:?}"
    );
    assert!(nags_row(&registry, &id).starts_with("paused until you resume them (by the operator"));
    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}
