//! The stuck-queue census, end to end: the model is WOKEN and handed the sentence. Every
//! queue transition is notified once as it happens; what nothing said was the TOTAL and the
//! AGE, so `mergequeue::stuck` counts them and `Sessions::deliver_due_censuses` says so.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_metrics::now_ms;
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::{SessionEvent, hub::Hub};
use letibot_tokencore::Vocab;
use letibot_tokencore::store::{
    Approval, MergePriority, MergeState, Move, NewReview, ReviewKind, ReviewState, SessionRecord,
    Store, TodoBy, TodoItem, TodoStatus,
};
use letibot_transcript::TranscriptItem;
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::qwen::a_plain_answer_turn;
use support::stub::{read_request, write_frames, write_props};

/// What the model answers - the mark of a turn that RAN rather than one that was merely opened.
const THE_ANSWER: &str = "census-answer-marker-read-it";

/// A temp directory, a real git repository in it, and a store - the census keys rows by the
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
    let ok = letibot_tools::testing::git(&repo)
        .args(["init", "--quiet"])
        .status()
        .expect("git runs");
    assert!(ok.success(), "a repository to key the queue by");
    letibot_tools::testing::repo_config(&repo);
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
    // Never this box's model server: every endpoint this file opens with is replaced by the stub
    // below, and a fixture that forgot to would refuse on port 1 rather than answer.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

/// A landing row in `to`, entered at `at` - born from a real approval, because that is the only
/// way the queue makes one (`Store::approve_review`).
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

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

/// A model that answers every turn the same way, and keeps the request it was asked.
struct Stub {
    endpoint: Endpoint,
    seen: Arc<Mutex<Vec<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    fn answering(frames: Vec<Frame>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mine = seen.clone();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // `/props` is not a turn. A session asking its endpoint for the window sends a
                // `GET`, and counting it as one would make every count here off by one.
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                mine.lock().unwrap().push(body);
                let _ = write_frames(stream, &frames);
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            seen,
            handle: Some(handle),
        }
    }

    /// How many turns have been asked for - the count a model call raises, read from the wire rather
    /// than from the hub, so it cannot agree with a bug in the harness.
    fn turns(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// What the model was handed for turn `n` - the request's token ids, detokenized with the
    /// vocabulary the session tokenized with. This is the assertion that separates a TURN from a
    /// transcript row: a notice never reaches here.
    fn prompt_of(&self, n: usize, vocab: &Vocab) -> String {
        let body = self
            .seen
            .lock()
            .unwrap()
            .get(n)
            .cloned()
            .unwrap_or_else(|| panic!("turn {n} was never asked for (turns: {})", self.turns()));
        let json: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("request {n} is JSON: {e}"));
        let ids: Vec<u32> = json["prompt"]
            .as_array()
            .unwrap_or_else(|| panic!("request {n} carries a prompt: {body}"))
            .iter()
            .map(|v| v.as_u64().expect("a token id") as u32)
            .collect();
        vocab
            .detokenize(&ids, true)
            .unwrap_or_else(|e| panic!("request {n}'s ids decode: {e}"))
    }

    /// Every prompt the model was handed, in order - a search over the whole run rather than one
    /// turn, so a sentence that arrived on a later turn is found where it belongs.
    #[allow(dead_code)]
    fn prompts(&self, vocab: &Vocab) -> Vec<String> {
        (0..self.turns())
            .map(|n| self.prompt_of(n, vocab))
            .collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        // Detach rather than join: the listener lives as long as the test, and joining would block on
        // a socket nobody will ever connect to again.
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

// ---------------------------------------------------------------------------
// Reading the log back
// ---------------------------------------------------------------------------

/// Every `Speaker::Agent` row on this session's log, as its text - what the person is drawn and
/// what the model reads at its next turn. It is NOT the evidence that a turn ran: only `turns` and
/// the stub's own record are.
fn agent_rows(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                TranscriptItem::User {
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

/// The model's own answers, which is how a turn that RAN is told from one that was merely opened.
fn answers(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                TranscriptItem::Assistant { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// THE TESTS
// ---------------------------------------------------------------------------

/// Eight stuck rows WOKE THE MODEL with the count and the age - once, then once an interval
/// later then not at all once the queue recovered. FAILS on a census written as a notice
/// and fails in the way that matters
#[test]
fn a_stuck_row_wakes_the_model_and_a_recovered_queue_runs_no_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, repo, key, id) = seeded("once");
    let path = dir.join("sessions.db");
    let store = Store::open(&path).expect("the store");
    store
        .put_session(&session_row(&id, &repo, None))
        .expect("the session row");
    // The eight rows: three `failed` and five `conflict`, four hours old - past `STUCK_AFTER_MS`
    // so the census is news the first time the daemon looks.
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

    // The vocabulary first - the canned frames must speak the ids the session will send.
    let dead = config(&path, &repo, &id);
    let parts = Parts::load(&dead).expect("the vocabulary must load");
    let frames = a_plain_answer_turn(&parts.vocab, "reading the queue", THE_ANSWER, 30);
    let stub = Stub::answering(frames);

    let cfg = config(&path, &repo, &id);
    let cfg = Config {
        endpoint: stub.endpoint.clone(),
        ..cfg
    };
    let (mut sessions, registry) = opened(&parts, &cfg, &id);
    // The production windows, shortened - the age gate is the fixture's (four hours); the two
    // clocks are shortened only so a test can see them tick.
    sessions.set_nag_window(Duration::from_secs(60));
    sessions.set_stuck_repeat(Duration::from_millis(200));

    // One delivery, and it is a TURN - the model was asked, and what it was asked is the
    // sentence: the count, the age and the verbs. The sweep is spent first, so "this door
    // armed it" is a measurement and not a leftover
    sessions.sweep_abandoned_calls();
    assert!(sessions.sweep_due_at().is_none(), "the fixture spent it");
    assert_eq!(sessions.deliver_due_censuses(), 1, "one session is told");
    assert!(
        sessions.sweep_due_at().is_some(),
        "a census turn must arm the sweep that closes out a call it stranded"
    );
    assert_eq!(stub.turns(), 1, "the census must reach the model");
    let said = stub.prompt_of(0, &parts.vocab);
    assert!(
        said.contains("[queue stuck]"),
        "the model's prompt carries the census: {said}"
    );
    assert!(said.contains("8 row(s)"), "the total nobody had: {said}");
    assert!(
        said.contains("5 conflict · 3 failed"),
        "and each bad state's own count: {said}"
    );
    assert!(
        said.contains("The oldest has been there 4h"),
        "and the age nobody had: {said}"
    );
    assert!(
        said.contains("/queue restart <id>") && said.contains("/queue rm <id>"),
        "a count with no verb is a statistic: {said}"
    );

    // And it landed where every other turn lands: the row the person reads, and the answer.
    let hub = registry.get(&id).expect("the session's hub");
    assert_eq!(censuses(&hub).len(), 1, "one row on the transcript");
    assert!(
        answers(&hub).iter().any(|a| a.contains(THE_ANSWER)),
        "the turn was ANSWERED, not merely opened: {:?}",
        answers(&hub)
    );

    // Three more passes inside the interval run no turn.
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(sessions.deliver_due_censuses(), 0, "nothing is due yet");
    }
    assert_eq!(stub.turns(), 1, "one model call, however many passes");
    assert_eq!(
        censuses(&registry.get(&id).unwrap()).len(),
        1,
        "and one row"
    );

    // Past the interval, a second one - the cadence, and the second model call.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sessions.deliver_due_censuses(), 1, "the repeat came due");
    assert_eq!(stub.turns(), 2, "a second call, an interval later");
    assert!(
        stub.prompt_of(1, &parts.vocab).contains("[queue stuck]"),
        "and it is the census again"
    );

    // And the rows recover. The next arming finds nothing stuck, and the clock stands down in
    // both directions: no turn now, and nothing armed after it.
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
            sessions.deliver_due_censuses(),
            0,
            "nothing stuck, nothing said"
        );
    }
    assert_eq!(
        stub.turns(),
        2,
        "a tree that goes quiet stops nagging — not one call per interval for ever"
    );
    assert!(
        sessions.census_due_at(&id).is_none(),
        "and nothing is armed for the next interval"
    );

    // And a SECOND incident - the defect the review found in the clock. A new row goes bad YOUNG
    // and a clock that has already said one census must still arm for this one's crossing.
    let store = Store::open(&path).expect("the store");
    a_landing(&store, &key, "m-second", MergeState::Failed, now_ms());
    drop(store);
    sessions.set_stuck_repeat(Duration::from_millis(1));
    assert_eq!(
        sessions.deliver_due_censuses(),
        0,
        "a row that went bad just now is the transition's news, not the census's"
    );
    let news = sessions.harness_of(&id).expect("open").queue_news();
    assert!(!news.says, "too young to be news: {news:?}");
    assert!(news.matures_in.is_some(), "and it will be news: {news:?}");
    assert!(
        sessions.census_due_at(&id).is_some(),
        "the crossing is ARMED — before the clock fix this was None, and the row sat there"
    );
    assert_eq!(stub.turns(), 2, "and no call has been spent on it yet");

    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The plan nag and the census cannot starve each other - the case that matters now that
/// both are turns, because a turn END re-arms both clocks.
#[test]
fn a_census_and_a_plan_check_do_not_postpone_each_other() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, repo, key, id) = seeded("both");
    let path = dir.join("sessions.db");
    let store = Store::open(&path).expect("the store");
    store
        .put_session(&session_row(&id, &repo, None))
        .expect("the session row");
    // An unfinished plan, so the check has something to ask about.
    store
        .put_todos(
            &id,
            &[TodoItem {
                content: "finish the migration".into(),
                status: TodoStatus::InProgress,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .expect("the todo row");
    a_landing(
        &store,
        &key,
        "m-failed-0",
        MergeState::Failed,
        now_ms().saturating_sub(4 * 3_600_000),
    );
    drop(store);

    let dead = config(&path, &repo, &id);
    let parts = Parts::load(&dead).expect("the vocabulary must load");
    let frames = a_plain_answer_turn(&parts.vocab, "reading it", THE_ANSWER, 30);
    let stub = Stub::answering(frames);
    let cfg = config(&path, &repo, &id);
    let cfg = Config {
        endpoint: stub.endpoint.clone(),
        ..cfg
    };
    let (mut sessions, registry) = opened(&parts, &cfg, &id);
    sessions.set_nag_window(Duration::from_millis(1));
    sessions.set_stuck_repeat(Duration::from_secs(600));

    // The census first, while the plan's check is already due - the order that could starve it.
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(sessions.deliver_due_censuses(), 1, "the census ran");
    assert_eq!(stub.turns(), 1);
    // One window later, not one census interval later: the plan's own deadline survived.
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        sessions.deliver_due_nags(),
        1,
        "a census turn must not push the plan's check out by the census's interval"
    );
    assert_eq!(stub.turns(), 2, "both turns reached the model");

    // And the other direction: the plan's check ran, and the census's deadline did not move -
    // it is an ANCHOR (`census_said_at` + the repeat), which no turn touches, so a nag turn cannot
    // push the census out the way a shared deadline would.
    let armed = sessions.census_due_at(&id);
    assert!(
        armed.is_some(),
        "the census is still armed after the plan's check ran"
    );
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        sessions.deliver_due_censuses(),
        0,
        "and it is not due yet — 600 s of anchor, not the next pass"
    );
    assert_eq!(
        sessions.census_due_at(&id),
        armed,
        "a plan nag turn must not move the census's deadline"
    );
    assert_eq!(stub.turns(), 2, "and it spent no model call doing it");

    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A subagent is not told. The census is the PROJECT's - a child told *eight rows are stuck
/// in `main`* has been handed somebody else's work, and the root-session rule is what keeps
/// it out. It runs no turn either
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

    let dead = config(&path, &repo, child);
    let parts = Parts::load(&dead).expect("the vocabulary must load");
    let frames = a_plain_answer_turn(&parts.vocab, "reading the queue", THE_ANSWER, 30);
    let stub = Stub::answering(frames);
    let cfg = config(&path, &repo, child);
    let cfg = Config {
        endpoint: stub.endpoint.clone(),
        ..cfg
    };
    let (mut sessions, registry) = opened(&parts, &cfg, child);
    sessions.set_stuck_repeat(Duration::from_millis(1));
    let h = sessions.harness_of(child).expect("the child is open");
    assert!(
        h.stuck_census().is_none(),
        "a child is told nothing about its project's queue"
    );
    assert!(!h.queue_news().says);
    assert!(h.stuck_census_text().is_none());
    assert_eq!(
        sessions.deliver_due_censuses(),
        0,
        "and no turn is spent on it"
    );
    assert_eq!(stub.turns(), 0, "the child's model was never asked");

    registry.close();
    let _ = std::fs::remove_dir_all(&dir);
}
