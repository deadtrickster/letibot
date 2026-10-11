//! **R79 between turns: nothing stays held once a turn has ended.**
//!
//! The operator, 2026-10-11, on the pg-noop session (*"[meanwhile] 1 result arrived while you were
//! busy …"* arriving the moment they sent a prompt, long after the model had gone idle): *"jobs
//! were ran for a reason and held for a reason. once turn ends no reason to hold"*. So a result
//! held while a turn ran is delivered as a turn of its own the moment that turn ends, a result
//! that arrives while the session is idle is delivered at once as an ordinary completion, and a
//! prompt that gets there before the turn-end wake still carries the batch.
//!
//! The settlement is put on the session's queue the way its watcher thread would
//! ([`letibot_harnessd::jobwatch::JobWatchers::queue_for_test`]); everything after that is the
//! daemon's own door — `Harness::wake`, `Harness::submit` — against the canned server the turn
//! crate's tests replay. It needs the vocabulary GGUF and not a model.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use letibot_harnessd::Parts;
use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Harness;
use letibot_harnessd::jobwatch::{BackgroundKind, JobCompletion};
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoItem, TodoNeed, TodoStatus};
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

#[allow(dead_code)]
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Reply;

const IM_END: u32 = 248046;
const THINK_CLOSE: u32 = 248069;

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A root session in its own scratch workspace — so the delivery record lands there — with
/// `todos` on its board.
fn session(tag: &str, todos: &[TodoItem]) -> (TempDir, Config, Parts) {
    let dir = TempDir::new(tag);
    let store = dir.path.join("sessions.db");
    let session_id = format!("{tag}-test");
    {
        let s = Store::open(&store).expect("seeding");
        s.put_session(&SessionRecord {
            id: session_id.clone(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: dir.path.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .expect("the session row");
        if !todos.is_empty() {
            s.put_todos(&session_id, todos).expect("the todo rows");
        }
    }
    let mut cfg = Config::for_this_box(&dir.path);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(store);
    cfg.session_id = session_id;
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    (dir, cfg, parts)
}

fn spoken(vocab: &letibot_tokencore::Vocab, ids: &[u32]) -> Vec<canned::Frame> {
    ids.iter()
        .map(|id| canned::Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

fn a_plain_answer(vocab: &letibot_tokencore::Vocab, answer: &str) -> Reply {
    let thought = vocab.tokenize_text("thinking").expect("tokenizes");
    let answer_ids = vocab.tokenize_text(answer).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: 30,
        processed: 30,
    }];
    frames.extend(spoken(vocab, &thought));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer_ids.len() + 2) as u64,
        n_prompt: 30,
        cache_n: 0,
    });
    Reply::Frames(frames)
}

fn settled(owner: &str, job: &str, state: &str) -> JobCompletion {
    JobCompletion {
        memory: None,
        kind: BackgroundKind::Job,
        owner: owner.into(),
        job: job.into(),
        command: format!("make {job}"),
        slug: None,
        state: state.into(),
        produced: 3,
        elapsed_ms: 2_000,
        detail: String::new(),
        arrived: SystemTime::now(),
    }
}

/// The user rows of the transcript, speaker and text.
fn user_rows(h: &Harness) -> Vec<(Speaker, String)> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { speaker, parts } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } => Some((*speaker, text.clone())),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

fn records(dir: &TempDir) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.path.join(letibot_harnessd::delivery::LOG))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("a record line is JSON"))
        .collect()
}

/// **The session's hub adopted into a registry, the way the daemon's are** — so the bell the
/// watcher set rings is one this test can ask (`Registry::next_work_until`) whether the session
/// was woken.
fn adopted(parts: &Parts, cfg: &Config) -> (Harness, Arc<Registry>) {
    let registry = Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    registry
        .adopt(
            hub.clone(),
            cfg.session_id.clone(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: cfg.workspace.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    let h = Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry.clone())
        .expect("the session opens");
    (h, registry)
}

/// Whether the worker has a wake waiting for `id`, without blocking.
fn woken(registry: &Registry, id: &str) -> bool {
    match registry.next_work_until(Some(Instant::now())) {
        WorkOrIdle::Work(Work::Woken(w)) => w == id,
        _ => false,
    }
}

/// **A turn that ends with results held wakes its session, and the wake delivers them as ONE
/// batch, as a turn of its own.**
///
/// pg-noop, 2026-10-11: a result held while the model worked reached it only when the operator
/// next spoke, under a heading that said *"while you were busy"* about a model that had been idle
/// for an hour. The operator's ruling: *"jobs were ran for a reason and held for a reason. once
/// turn ends no reason to hold"*. Two results held during a turn (the round boundary's hold, as
/// `HubSteering` does it), the turn ends (`JobWatchers::turn_ended`, what both doors into
/// `run_rounds` call): the bell names the session, `Harness::wake` runs a turn, the batch opens
/// with the turn-end heading, and the record says `turn_end`, two together.
#[test]
fn a_turn_that_ends_with_results_held_delivers_them_as_its_own_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, mut cfg, parts) = session("r79-turn-end", &[]);
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "noted")], 1);
    cfg.endpoint = serv.endpoint.clone();
    let (mut h, registry) = adopted(&parts, &cfg);
    let w = h.job_watchers().expect("a watcher set").clone();

    w.hold(vec![
        settled(&cfg.session_id, "j21", "exited 0"),
        settled(&cfg.session_id, "j22", "exited 1"),
    ]);
    assert!(
        !woken(&registry, &cfg.session_id),
        "held mid-turn rings nothing"
    );
    w.turn_ended();
    assert!(
        woken(&registry, &cfg.session_id),
        "a turn that ended with results held must wake its session"
    );

    assert!(
        h.wake().expect("the wake runs").is_some(),
        "the held batch is a turn of its own once the turn has ended"
    );
    let rows = user_rows(&h);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, Speaker::Agent);
    assert!(
        rows[0].1.starts_with(
            "[meanwhile] 2 results arrived while you were working — nothing in your plan was \
             waiting on them, so they were held until your turn ended instead of interrupting you."
        ) && rows[0].1.contains("`j21` exited 0")
            && rows[0].1.contains("`j22` exited 1"),
        "{}",
        rows[0].1
    );
    assert_eq!(w.held_count(), 0);
    let rec = records(&dir);
    assert_eq!(rec.len(), 2, "{rec:?}");
    assert!(
        rec.iter()
            .all(|r| r["rule"] == "turn_end" && r["batch"] == 2),
        "{rec:?}"
    );
}

/// **A result that arrives while the session is idle is not held at all**: the wake its arrival
/// rang delivers it at once, as an ordinary `[job]` notice with no `[meanwhile]` heading, recorded
/// as `idle`.
///
/// pg-noop, 2026-10-11, and the operator's ruling: *"jobs were ran for a reason and held for a
/// reason. once turn ends no reason to hold"* — an idle model is interrupted by nothing, so there
/// is no break to wait for and nothing to explain.
#[test]
fn a_completion_that_arrives_while_idle_is_delivered_at_once_without_a_heading() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, mut cfg, parts) = session("r79-idle", &[]);
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "noted")], 1);
    cfg.endpoint = serv.endpoint.clone();
    let (mut h, _registry) = adopted(&parts, &cfg);

    h.job_watchers()
        .expect("a watcher set")
        .queue_for_test(settled(&cfg.session_id, "j31", "exited 0"));
    assert!(
        h.wake().expect("the wake runs").is_some(),
        "an idle session hears a routine completion at once"
    );
    let rows = user_rows(&h);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].1.starts_with("[job] ") && rows[0].1.contains("`j31` exited 0"),
        "{}",
        rows[0].1
    );
    assert!(!rows[0].1.contains("[meanwhile]"), "{}", rows[0].1);
    let rec = records(&dir);
    assert_eq!(rec.len(), 1, "{rec:?}");
    assert_eq!(rec[0]["rule"], "idle");
    assert_eq!(rec[0]["handle"], "j31");
}

/// **A prompt that gets there before the turn-end wake carries the batch, and the wake then runs
/// nothing** — one delivery, never two. Commands drain before wakes (`Bell::next_any`), so this is
/// the order the worker takes them in when the operator types the moment a turn ends.
///
/// pg-noop, 2026-10-11, and the operator's ruling: *"jobs were ran for a reason and held for a
/// reason. once turn ends no reason to hold"* — the batch is still what arrived while the model
/// was busy, so it keeps the batch heading and sits BEFORE the operator's words.
#[test]
fn a_prompt_before_the_turn_end_wake_carries_the_batch_and_the_wake_runs_nothing() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, mut cfg, parts) = session("r79-root", &[]);
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "noted")], 1);
    cfg.endpoint = serv.endpoint.clone();
    let (mut h, _registry) = adopted(&parts, &cfg);
    let w = h.job_watchers().expect("a watcher set").clone();
    w.hold(vec![settled(&cfg.session_id, "j21", "exited 0")]);
    w.turn_ended();

    h.submit("what is next?").expect("the prompt's turn runs");
    let rows = user_rows(&h);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(
        rows[0].0,
        Speaker::Agent,
        "the batch is the harness's, never the operator's"
    );
    assert!(
        rows[0]
            .1
            .starts_with("[meanwhile] 1 result arrived while you were busy")
            && rows[0].1.contains("`j21` exited 0"),
        "{}",
        rows[0].1
    );
    assert_eq!(
        rows[1],
        (Speaker::Operator, "what is next?".to_string()),
        "the operator's words stay the last thing the model reads"
    );
    assert!(
        h.wake().expect("the wake runs").is_none(),
        "the batch was delivered once; the turn-end wake has nothing left"
    );
    let rec = records(&dir);
    assert_eq!(rec.len(), 1, "{rec:?}");
    assert_eq!(rec[0]["rule"], "batch");
    assert_eq!(rec[0]["handle"], "j21");
}

/// **A completion the plan `needs` is said first, and the batch held during the last turn rides
/// in after it** — under the turn-end heading, since the wake that runs is the one the turn's end
/// is owed anyway (the operator, 2026-10-11: *"jobs were ran for a reason and held for a reason.
/// once turn ends no reason to hold"*).
#[test]
fn a_needed_completion_wakes_a_root_and_the_held_batch_rides_along() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, mut cfg, parts) = session(
        "r79-needed",
        &[TodoItem {
            content: "ship once the build is green".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: vec![TodoNeed::Job {
                handle: "j30".into(),
            }],
        }],
    );
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "on it")], 1);
    cfg.endpoint = serv.endpoint.clone();
    let (mut h, _registry) = adopted(&parts, &cfg);

    let w = h.job_watchers().expect("a watcher set").clone();
    w.hold(vec![settled(&cfg.session_id, "j31", "exited 0")]);
    w.turn_ended();
    std::thread::sleep(std::time::Duration::from_millis(5));
    w.queue_for_test(settled(&cfg.session_id, "j30", "exited 0"));
    assert!(
        h.wake().expect("the wake runs").is_some(),
        "the plan needs j30, so it is a turn"
    );
    let rows = user_rows(&h);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let text = &rows[0].1;
    let needed = text.find("`j30`").expect("the needed job is told");
    let batch = text
        .find("[meanwhile] 1 result arrived while you were working")
        .expect("the batch rides along");
    assert!(needed < batch, "what the plan waits on first: {text}");
    assert!(text[batch..].contains("`j31`"), "{text}");
    let rules: Vec<(String, String)> = records(&dir)
        .iter()
        .map(|r| {
            (
                r["handle"].as_str().unwrap().to_string(),
                r["rule"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rules,
        [
            ("j30".to_string(), "immediate:needed".to_string()),
            ("j31".to_string(), "turn_end".to_string())
        ]
    );
}
