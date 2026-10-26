//! **A re-seat writes nothing it has not earned, and a fork is the session's only once its rows
//! are stored** (TODO R91.2, R91.13).
//!
//! * R91.13 — `reseat_target` wrote a `stable_prefix` row (and moved `cfg.system`) before
//!   `reingest`'s fit check, so a carry refused as over the window left an orphan prompt row on
//!   every attempt.
//! * R91.2 — `fork_onto` registered the new transcript (`put_fork`, the newest, so what a resume
//!   opens) before writing a row of it. A store that failed mid-carry left a resume onto a partial
//!   conversation; an error after the swap returned through `reingest`'s refusal path before
//!   `self.prefix = next`, and the operator was told "Nothing was changed".

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{ForkTail, Harness};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::Store;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};
use letibot_turn::CompactionOutcome;

#[allow(dead_code)]
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

const IM_END: u32 = 248046;
const THINK_CLOSE: u32 = 248069;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "harnessd-reseat-writes-{tag}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(store: &std::path::Path, session_id: &str, workspace: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn a_conversation(n: usize) -> Vec<TranscriptItem> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                TranscriptItem::User {
                    speaker: Speaker::Operator,
                    parts: vec![UserPart::Text {
                        text: format!("question number {i}: what does the log mode do next?"),
                    }],
                }
            } else {
                TranscriptItem::Assistant {
                    text: format!("answer number {i}: it tails the file and follows appends."),
                    tool_calls: vec![],
                    truncated: false,
                }
            }
        })
        .collect()
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

fn a_plain_answer(vocab: &letibot_tokencore::Vocab, answer: &str) -> canned::Reply {
    let answer_ids = vocab.tokenize_text(answer).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: 30,
        processed: 30,
    }];
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (answer_ids.len() + 2) as u64,
        n_prompt: 30,
        cache_n: 0,
    });
    canned::Reply::Frames(frames)
}

/// A session seated under an older prompt with `rows` rows behind it, so the prompt this daemon
/// composes is a different one and a re-seat has something to do.
fn a_stale_session(parts: &Parts, seated: &Config, rows: usize) {
    let mut older = seated.clone();
    older.system = "the prompt this session was seated with, several builds ago".into();
    let hub = Hub::new(&seated.session_id);
    let registry = Registry::of(hub.clone());
    let mut h = Harness::open_with_registry(parts, older, hub, None, None, registry)
        .expect("the session opens");
    let outcome = CompactionOutcome {
        turn_id: format!("{}#seed", h.transcript_id()),
        summary: "the work so far".into(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
    };
    let history = a_conversation(rows);
    h.fork_to_summary(
        &outcome,
        None,
        None,
        ForkTail {
            items: &history,
            split: None,
            because: "",
        },
    )
    .expect("the seed fork lands");
}

fn count(store: &std::path::Path, sql: &str) -> i64 {
    Store::open(store)
        .expect("a second connection")
        .connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("the count")
}

/// **R91.13: a re-seat refused as over the window writes no prompt row.**
#[test]
fn a_reseat_refused_over_the_window_writes_no_prompt_row() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("over");
    let ws = TempDir::new("over-ws");
    let store = dir.0.join("sessions.db");
    let seated = config(&store, "s-reseat-over", &ws.0);
    let parts = Parts::load(&seated).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    a_stale_session(&parts, &seated, 40);

    let mut now = seated.clone();
    // Smaller than the conversation, so a verbatim carry is over the window.
    now.context_window = Some(64);
    let hub = Hub::new(&now.session_id);
    let registry = Registry::of(hub.clone());
    let mut h = Harness::open_with_registry(&parts, now, hub, None, None, registry)
        .expect("the session reopens");
    let rows_before = count(&store, "SELECT COUNT(*) FROM stable_prefix");
    let system_before = h.config().system.clone();

    let Err(err) = h.reingest() else {
        panic!("over the window, the carry is refused");
    };
    assert!(err.to_string().contains("Nothing was changed"), "{err}");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM stable_prefix"),
        rows_before,
        "a refused re-seat left a prompt row behind"
    );
    assert_eq!(h.config().system, system_before);
}

/// **R91.2: a fork whose rows the store refused is not what a restart opens, and the re-seat is
/// not reported as "Nothing was changed".** The store refuses by having `transcript_item` moved
/// away through a second connection while the carry is written; once it is back, the next store
/// write takes the rows up and seals the fork.
#[test]
fn a_fork_the_store_refused_is_not_what_a_restart_opens() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("unstored");
    let ws = TempDir::new("unstored-ws");
    let store = dir.0.join("sessions.db");
    let mut seated = config(&store, "s-reseat-unstored", &ws.0);
    let parts = Parts::load(&seated).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    a_stale_session(&parts, &seated, 40);

    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "fine")], 1);
    seated.endpoint = serv.endpoint.clone();
    let hub = Hub::new(&seated.session_id);
    let registry = Registry::of(hub.clone());
    let mut h =
        Harness::open_with_registry(&parts, seated.clone(), hub.clone(), None, None, registry)
            .expect("the session reopens");
    let old = h.transcript_id().to_string();

    let other = Store::open(&store).expect("a second connection");
    other
        .connection()
        .execute_batch("ALTER TABLE transcript_item RENAME TO transcript_item_away")
        .expect("the table moves away");
    let r = h.reingest();
    other
        .connection()
        .execute_batch("ALTER TABLE transcript_item_away RENAME TO transcript_item")
        .expect("the table comes back");
    let r = r.expect("past the swap the re-seat happened; it is not a refusal");
    assert_ne!(h.transcript_id(), old, "the session speaks the fork");
    assert_eq!(r.fork.parent_id, old);
    assert!(
        hub.retained().iter().any(|e| matches!(
            &e.event,
            SessionEvent::Warning { code, .. } if code == "fork_unstored"
        )),
        "the gap was not said"
    );
    assert_eq!(
        other
            .current_transcript_id(&seated.session_id)
            .unwrap()
            .as_deref(),
        Some(old.as_str()),
        "a fork with none of its rows stored is what a restart would open"
    );
    // A restart now resumes the parent, whole — and **the restart is READ FROM THE STORE here
    // rather than run**: a second harness for one session is refused by name (the writer claim,
    // `harnessd::writer`), because the first one is still alive and holding the fork this test is
    // about. `current_transcript_id` IS the restart's answer — the row a resume opens — and the
    // reopen itself is exercised where a daemon really stops: `restart_restore`.
    let refusal = {
        let hub = Hub::new(&seated.session_id);
        let registry = Registry::of(hub.clone());
        Harness::open_with_registry(&parts, seated.clone(), hub, None, None, registry)
            .err()
            .expect("a second harness for one session is refused")
            .to_string()
    };
    assert!(
        refusal.contains("is being written by another process"),
        "and the refusal says why: {refusal}"
    );

    // The next store write takes the rows up and seals the fork.
    h.submit("carry on").expect("the turn runs");
    let new = h.transcript_id().to_string();
    assert_eq!(
        other
            .current_transcript_id(&seated.session_id)
            .unwrap()
            .as_deref(),
        Some(new.as_str()),
        "once its rows are stored the fork is the conversation"
    );
}
