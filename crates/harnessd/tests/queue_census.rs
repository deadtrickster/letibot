//! **The stuck-queue census, end to end through a real `Harness` and a real store** — the count
//! read off the queue, the sentence the model is actually sent, and the clock that stops.
//!
//! The operator's report (2026-11-01): eight merge-queue rows sat stuck for hours, three `failed`
//! and five `conflict`, and every one of those transitions *was* notified once as it happened.
//! What nothing said was the TOTAL and the AGE. `mergequeue::stuck` counts them and
//! `Sessions::note_due_censuses` says so — this is the seam, on a store, a real git repository and
//! the daemon's own idle pass.
//!
//! # What this needs, and what it does NOT need
//!
//! The vocabulary GGUF (a `Harness` renders its stable prefix at open) and **not a model**. The
//! census is a notice and not a turn — `Harness::note_to_model` appends one transcript row and
//! returns — so this file asserts the row arrives while `deliver_due_nags` (the only door that
//! spends a model call) delivers nothing at all, and `turns()` below is the count of rounds the hub
//! ever saw: zero.

use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::{SessionEvent, hub::Hub};
use letibot_tokencore::store::{
    Approval, MergePriority, MergeState, Move, NewReview, ReviewKind, ReviewState, SessionRecord,
    Store,
};
use letibot_turn::Endpoint;

/// **A temp directory, a real git repository in it, and a store** — the census keys rows by the
/// repository git resolves, so a fixture that named a path that is not one would measure nothing.
fn seeded(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "harnessd-queue-census-{tag}-{}-{}-{}",
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
    let repo = dir.join("project");
    std::fs::create_dir_all(&repo).unwrap();
    let ok = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .expect("git runs");
    assert!(ok.success(), "a repository to key the queue by");
    let key = std::fs::canonicalize(&repo).unwrap().display().to_string();
    (dir, repo, key, "s-project".to_string())
}

fn config(store: &std::path::Path, workspace: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // **Never this box's model server**, and no ladder: if anything here ever spends a turn, this
    // file should fail loudly rather than hang. `turns()` is the assertion that it does not.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

/// **A landing row in `to`, entered at `at`** — born from a real approval, because that is the
/// only way the queue makes one (`Store::approve_review`).
fn a_landing(store: &Store, repo: &str, id: &str, to: MergeState, at: u64) {
    let review = format!("r-{id}");
    store
        .enqueue_review(
            &NewReview {
                id: review.clone(),
                repository: repo.to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-child".into(),
                branch: format!("agent/{id}"),
                worktree: None,
                base_sha: "base".into(),
                head_sha: "head".into(),
                brief: "a branch".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            at,
        )
        .unwrap();
    store
        .move_review(
            &review,
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new("test", "a reviewer has it"),
            at,
        )
        .unwrap();
    store
        .approve_review(
            &review,
            &Approval {
                expect: ReviewState::Reviewing,
                expect_owner: None,
                reviewed_sha: "head".into(),
                by: "reviewer:one".into(),
                reasons: vec!["fine".into()],
                files: vec![],
                commands: vec![],
                why: "approved".into(),
                merge_id: id.to_string(),
            },
            at,
        )
        .unwrap();
    store
        .move_merge(
            id,
            MergeState::Queued,
            to,
            &Move::new("test", "the gate"),
            at,
        )
        .unwrap();
}

fn now_ms() -> u64 {
    letibot_sessionlog::registry::now_ms()
}

/// **Every `Speaker::Agent` row on this session's log, as its text** — the shape a notice AND a nag
/// turn both take. What tells them apart is `turns()`: only a turn starts a round.
fn agent_rows(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                letibot_transcript::TranscriptItem::User {
                    speaker: letibot_transcript::Speaker::Agent,
                    parts,
                } => Some(
                    parts
                        .iter()
                        .filter_map(|p| match p {
                            letibot_transcript::UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn censuses(hub: &Hub) -> Vec<String> {
    agent_rows(hub)
        .into_iter()
        .filter(|t| t.contains("[queue stuck]"))
        .collect()
}

/// **How many rounds this session ever started** — the count a model call would raise.
fn turns(hub: &Hub) -> usize {
    hub.retained()
        .iter()
        .filter(|e| matches!(e.event, SessionEvent::TurnStarted { .. }))
        .count()
}

fn session_row(id: &str, workspace: &std::path::Path, parent: Option<&str>) -> SessionRecord {
    SessionRecord {
        id: id.to_string(),
        title: Some("the project".into()),
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: workspace.display().to_string(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: parent.map(|p| p.to_string()),
    }
}

fn opened<'a>(
    parts: &'a Parts,
    cfg: &Config,
    id: &str,
) -> (Sessions<'a>, std::sync::Arc<Registry>) {
    let registry = Registry::new();
    registry
        .create(id, "", Sessions::wiring(cfg))
        .expect("the session is in the registry");
    let sessions = Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("it opens");
    (sessions, registry)
}

/// **Eight rows, one notice, a second one an interval later, and silence after** — the count and
/// the age are said, no turn is spent saying them, the passes inside the interval say nothing, and
/// a queue that recovers stands the clock down in both directions.
///
/// FAILS before this change: nothing counted the rows, so no such row was ever written and no
/// census deadline existed — the eight rows would have sat there, as they did.
#[test]
fn the_queue_is_counted_once_and_goes_quiet_when_it_recovers() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, repo, key, id) = seeded("once");
    let path = dir.join("sessions.db");
    let store = Store::open(&path).expect("the store");
    store
        .put_session(&session_row(&id, &repo, None))
        .expect("the session row");
    // **The operator's own eight**: three `failed` and five `conflict`, four hours old — past
    // `STUCK_AFTER_MS`, so the census is news the first time the daemon looks.
    let four_hours_ago = now_ms().saturating_sub(4 * 3_600_000);
    for i in 0..3 {
        a_landing(
            &store,
            &key,
            &format!("m-failed-{i}"),
            MergeState::Failed,
            four_hours_ago,
        );
    }
    for i in 0..5 {
        a_landing(
            &store,
            &key,
            &format!("m-conflict-{i}"),
            MergeState::Conflict,
            four_hours_ago,
        );
    }
    drop(store);

    let cfg = config(&path, &repo, &id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let (mut sessions, registry) = opened(&parts, &cfg, &id);
    // **The production windows, shortened** — the age gate is the fixture's (four hours); the two
    // clocks are shortened only so a test can see them tick.
    sessions.set_nag_window(Duration::from_secs(60));
    sessions.set_stuck_repeat(Duration::from_millis(200));

    // **One delivery**, and the sentence is the count, the age and the verbs.
    assert_eq!(sessions.note_due_censuses(), 1, "one session is told");
    let hub = registry.get(&id).expect("the session's hub");
    let said = censuses(&hub);
    assert_eq!(said.len(), 1, "one census: {said:#?}");
    assert!(
        said[0].contains("8 row(s)"),
        "the total nobody had: {}",
        said[0]
    );
    assert!(
        said[0].contains("5 conflict · 3 failed"),
        "and each bad state's own count: {}",
        said[0]
    );
    assert!(
        said[0].contains("The oldest has been there 4h"),
        "and the age nobody had: {}",
        said[0]
    );
    assert!(
        said[0].contains("/queue restart <id>") && said[0].contains("/queue rm <id>"),
        "a count with no verb is a statistic: {}",
        said[0]
    );

    // **And it cost no model call.** The board is empty, so the plan check has nothing to say —
    // and the round count is zero: `note_to_model` appended a row and returned.
    assert_eq!(sessions.deliver_due_nags(), 0, "no nag turn was due");
    assert_eq!(turns(&hub), 0, "and no round was ever started");

    // **Three more passes inside the interval say nothing.**
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(sessions.note_due_censuses(), 0, "nothing is due yet");
    }
    assert_eq!(
        censuses(&registry.get(&id).unwrap()).len(),
        1,
        "one notice, however many passes"
    );

    // **Past the interval, a second one** — the cadence the brief asked for. That the age it
    // composes has grown is `mergequeue::stuck::tests`' (the sentence is a pure function of the
    // census and the clock); what this asserts is that there ARE two deliveries, an interval apart.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sessions.note_due_censuses(), 1, "the repeat came due");
    assert_eq!(
        censuses(&registry.get(&id).unwrap()).len(),
        2,
        "two notices, one interval apart"
    );
    assert_eq!(turns(&hub), 0, "still no model call");

    // **And the rows recover.** The next arming finds nothing stuck, and the clock stands down in
    // both directions: no notice now, and nothing armed after it.
    let store = Store::open(&path).expect("the store");
    for i in 0..3 {
        store
            .move_merge(
                &format!("m-failed-{i}"),
                MergeState::Failed,
                MergeState::Withdrawn,
                &Move::new("operator:s-1", "not worth landing"),
                now_ms(),
            )
            .expect("the row moves");
    }
    for i in 0..5 {
        store
            .move_merge(
                &format!("m-conflict-{i}"),
                MergeState::Conflict,
                MergeState::Withdrawn,
                &Move::new("operator:s-1", "not worth landing"),
                now_ms(),
            )
            .expect("the row moves");
    }
    drop(store);

    sessions.set_stuck_repeat(Duration::from_millis(1));
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            sessions.note_due_censuses(),
            0,
            "nothing stuck, nothing said"
        );
    }
    assert_eq!(
        censuses(&registry.get(&id).unwrap()).len(),
        2,
        "a tree that goes quiet stops nagging — not one notice per interval for ever"
    );
    assert!(
        sessions.next_nag_at().is_none(),
        "and nothing is armed for the next interval"
    );

    // **And a SECOND incident** — the defect the review found. A new row goes bad YOUNG, and a
    // clock that has already said one census must still arm for this one's crossing. The first
    // incident after each daemon start worked and every later one did not, because the anchor of
    // the census that was said outlived the rows it was about.
    let store = Store::open(&path).expect("the store");
    a_landing(&store, &key, "m-second", MergeState::Failed, now_ms());
    drop(store);
    // The arming a transition's own wake would make.
    sessions.set_stuck_repeat(Duration::from_millis(1));
    assert_eq!(
        sessions.note_due_censuses(),
        0,
        "a row that went bad just now is the transition's news, not the census's"
    );
    let news = sessions.harness_of(&id).expect("open").queue_news();
    assert!(!news.says, "too young to be news: {news:?}");
    assert!(news.matures_in.is_some(), "and it will be news: {news:?}");
    assert!(
        sessions.next_nag_at().is_some(),
        "the crossing is ARMED — before the fix this was None, and the row sat there for ever"
    );
    assert_eq!(
        censuses(&registry.get(&id).unwrap()).len(),
        2,
        "and nothing has been said about it yet"
    );
}

/// **A subagent is not told.** The census is the PROJECT's — a child told *eight rows are stuck in
/// `main`* has been handed somebody else's work, and the root-session test is what keeps it out.
#[test]
fn a_child_session_gets_no_census() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, repo, key, root) = seeded("child");
    let path = dir.join("sessions.db");
    let child = "s-project-sub-1";
    let store = Store::open(&path).expect("the store");
    store
        .put_session(&session_row(&root, &repo, None))
        .expect("the root's row");
    store
        .put_session(&session_row(child, &repo, Some(&root)))
        .expect("the child's row");
    a_landing(
        &store,
        &key,
        "m-failed-0",
        MergeState::Failed,
        now_ms().saturating_sub(4 * 3_600_000),
    );
    drop(store);

    let cfg = config(&path, &repo, child);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let (sessions, _registry) = opened(&parts, &cfg, child);
    let h = sessions.harness_of(child).expect("the child is open");
    assert!(
        h.stuck_census().is_none(),
        "a child is told nothing about its project's queue"
    );
    assert!(!h.queue_news().says);
    assert!(h.stuck_census_text().is_none());
}
