//! **R79 between turns: a routine completion never starts a turn of its own** — and the next
//! turn the model takes anyway carries it, while a result the plan needs still wakes the session.
//!
//! The settlement is put on the session's queue the way its watcher thread would
//! ([`letibot_harnessd::jobwatch::JobWatchers::queue_for_test`]); everything after that is the
//! daemon's own door — `Harness::wake`, `Harness::submit` — against the canned server the turn
//! crate's tests replay. It needs the vocabulary GGUF and not a model.

use std::time::SystemTime;

use letibot_harnessd::Parts;
use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Harness;
use letibot_harnessd::jobwatch::{BackgroundKind, JobCompletion};
use letibot_sessionlog::hub::Hub;
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

/// **A root session does not get a wake turn for a routine completion** — the wake answers
/// `Ok(None)` and the canned server is never asked — and the next prompt carries it: one
/// `[meanwhile]` row in the harness's voice, BEFORE the operator's words, recorded as a batch.
#[test]
fn a_routine_completion_takes_no_wake_turn_and_rides_in_with_the_next_prompt() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, mut cfg, parts) = session("r79-root", &[]);
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "noted")], 1);
    cfg.endpoint = serv.endpoint.clone();
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub).expect("the session opens");

    h.job_watchers()
        .expect("a session has a watcher set")
        .queue_for_test(settled(&cfg.session_id, "j21", "exited 0"));
    let woke = h.wake().expect("the wake runs");
    assert!(
        woke.is_none(),
        "a routine completion started a turn of its own"
    );
    assert!(user_rows(&h).is_empty(), "nothing was said to the model");

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
    let rec = records(&dir);
    assert_eq!(rec.len(), 1, "{rec:?}");
    assert_eq!(rec[0]["rule"], "batch");
    assert_eq!(rec[0]["handle"], "j21");
}

/// **A completion the plan `needs` still wakes a root**, between turns — and since that turn runs
/// anyway, the routine one held beside it rides in with it, after the immediate one.
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
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub).expect("the session opens");

    let w = h.job_watchers().expect("a watcher set").clone();
    w.queue_for_test(settled(&cfg.session_id, "j31", "exited 0"));
    assert!(h.wake().expect("the wake runs").is_none(), "j31 is routine");
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
        .find("[meanwhile] 1 result")
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
            ("j31".to_string(), "batch".to_string())
        ]
    );
}
