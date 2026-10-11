//! **The operator's `/nag pause`, end to end through the daemon's own door** — the verb as a head
//! sends it (`CommandKind::Slash`), the clock it stands down, the hub rows that make it loud, the
//! settings row a head draws it from, and the session row that carries it across a restart.
//!
//! The operator: *"and i need a way to stop todo nags"*, then *"or rather - pause and the pause
//! must be loud"*. The clock's own reading of a pause is pinned with the times injected in
//! `sessions/idle_nag.rs`; what only this crate can assert is the seam — that the verb reaches the
//! clock, that a delivery pass runs no turn for a paused session, that a pause with an end is
//! LIFTED by the pass that finds it over, and that a resume comes back paused.
//!
//! # What this needs, and what it does not
//!
//! The vocabulary GGUF (a `Harness` renders its stable prefix at open) and **not a model**: the
//! endpoint is a port nothing listens on, with no retries — a turn that ran would fail at once,
//! and the assertions are that none runs.

use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::Outcome;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::hub::{CommandKind, Hub, QueuedCommand};
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::{NagPause, SessionEvent};
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoItem, TodoStatus};
use letibot_turn::Endpoint;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // **Never this box's model server**, and no ladder: a turn that ran would be the defect, and
    // it should fail at once rather than hide behind doubling waits.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

/// A store holding one session with an unfinished plan — `todos.rs`'s fixture.
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

/// Whether a `[todo check]` turn was started on this hub — the self-addressed prompt a delivered
/// check IS (`Speaker::Agent`).
fn checked(hub: &Hub) -> bool {
    hub.retained().iter().any(|e| {
        matches!(
            &e.event,
            SessionEvent::TranscriptContent { item, .. }
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

/// **`/nag pause` stops the arming AND the delivery** — the clock stands down, a delivery pass
/// whose window has long passed runs no turn, and an operator's added row (R77's sooner check)
/// arms nothing either. The hub says so, and so does the settings row.
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

    // **The operator's own row still lands on the board** — and earns no check while paused.
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

    // **Loud**: the hub row, the verb's own answer, and the settings row.
    assert_eq!(rows(&hub, "nags_paused").len(), 1, "one `nags_paused` row");
    assert!(
        rows(&hub, "slash")
            .iter()
            .any(|r| r.starts_with("/nag pause\n") && r.contains("PAUSED")),
        "the verb answers on the log: {:?}",
        rows(&hub, "slash")
    );
    assert_eq!(nags_row(&registry, &id), "paused until you resume them");

    // **And `/nag resume` puts the clock back.**
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

/// **`/nag pause 1s` turns itself back on** — the clock's one deadline is the pause's end, and the
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

/// **The pause survives a restart and a resume** — it is on the session's row, so the daemon that
/// opens the session next comes back paused, with nothing armed and the settings row saying so;
/// and a pause whose end passed while nobody held the session comes back OVER, and says it.
#[test]
fn a_pause_survives_a_resume_and_one_that_ran_out_while_closed_is_said() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, path, id) = seeded("resume");
    let parts = load_parts(&path, &id);
    {
        let (mut sessions, registry) = opened(&parts, &path, &id);
        slash(&mut sessions, &id, "nag pause 2h");
        registry.close();
    }
    let stored = Store::open(&path)
        .expect("the store")
        .session(&id)
        .expect("read")
        .expect("the row")
        .nags_paused_until;
    assert!(
        matches!(stored, Some(ms) if ms > 0),
        "the pause is on the session's row: {stored:?}"
    );

    // **The next daemon.**
    {
        let (sessions, registry) = opened(&parts, &path, &id);
        assert!(
            matches!(
                sessions.harness_of(&id).expect("open").nag_pause(),
                Some(NagPause::Until { .. })
            ),
            "the resumed session is still paused"
        );
        let end = sessions
            .next_nag_at()
            .expect("the pause's end is armed")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            end > Duration::from_secs(3600),
            "**and nothing sooner** — the open's own arming is the pause's end, not the plan's \
             minute: {end:?}"
        );
        assert!(
            nags_row(&registry, &id).starts_with("paused until "),
            "the settings row says it: {}",
            nags_row(&registry, &id)
        );
        // **The picker's row, off the store, says it too** — through the store source the daemon
        // installs at startup (`cli.rs`), which this fixture installs the same way.
        registry.set_source(
            letibot_harnessd::sessions::StoreSessions::open(&config(&path, &id))
                .expect("a store source"),
        );
        let brief = registry
            .list()
            .into_iter()
            .find(|b| b.session_id == id)
            .expect("listed");
        assert!(matches!(brief.nags_paused, Some(NagPause::Until { .. })));
        registry.close();
    }

    // **A pause that ran out while nobody held the session.**
    Store::open(&path)
        .expect("the store")
        .set_nags_paused_until(&id, Some(1_000))
        .expect("an end long past");
    {
        let (sessions, registry) = opened(&parts, &path, &id);
        assert!(
            sessions
                .harness_of(&id)
                .expect("open")
                .nag_pause()
                .is_none(),
            "a pause whose end has passed is over, not restored"
        );
        let hub = registry.get(&id).expect("the session's hub");
        let said = rows(&hub, "nags_resumed");
        assert!(
            said.len() == 1 && said[0].contains("while the session was closed"),
            "and the open says so: {said:?}"
        );
        assert!(sessions.next_nag_at().is_some(), "the check is armed again");
        registry.close();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
