//! **A `/reseat` reaches the model, in a turn of its own.**
//!
//! The operator, on pg-noop after re-seating it: *"since reseated nag reaches the model and it
//! starts nagging me - the result - that reseat happened shall reach the model too. and it
//! should start the turn"*. The stale-prompt notice reaches the model through the plan-check's
//! turn and tells it to keep asking for `/reseat` until the person acts; when the person did,
//! the act reached only the head (the `reseated` warning) and a system row at the top of a
//! forked transcript that no turn ran to read. So the model went on asking.
//!
//! Driven through the daemon's own arm — `Sessions::dispatch` taking a `Reseat` command — on a
//! session seated under one prompt and reopened by a daemon that composes another, which is
//! the restart that started this. The model is the canned server the turn crate's tests
//! replay. It needs the vocabulary GGUF and not a model.

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{ForkTail, Harness};
use letibot_harnessd::sessions::{Outcome, Sessions};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, DAEMON_SUBMITTER, Hub, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionWiring};
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
            "harnessd-reseat-turn-{tag}-{}-{}",
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
    // The operator's own `providers.toml` and `permission.json` are not this test's business.
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
    canned::Reply::Frames(frames)
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// **The model is told it was re-seated, and answers — and the stale notice is gone.**
///
/// Asserted on the conversation itself: after the re-seat there is a row in the harness's own
/// voice (`Speaker::Agent`, never the operator's words) that says what happened and tells the
/// model to stop asking for `/reseat`, and the model's answer follows it — a turn that RAN,
/// not a row left for whenever somebody next speaks.
#[test]
fn a_reseat_tells_the_model_and_starts_its_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("store");
    let ws = TempDir::new("ws");
    let store = dir.0.join("sessions.db");
    let id = "s-reseat-turn";
    let seated = config(&store, id, &ws.0);
    let parts = Parts::load(&seated).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // Seated under an older prompt, with a row behind it so the next open resumes. Opened
    // straight through `Harness`, so the prompt is exactly this text — the daemon below
    // composes its own from `prompts.toml`, which is the build that moved on.
    {
        let mut older = seated.clone();
        older.system = "the prompt this session was seated with, several builds ago".into();
        let hub = Hub::new(id);
        let registry = Registry::of(hub.clone());
        let mut h = Harness::open_with_registry(&parts, older, hub, None, None, registry)
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
        h.fork_to_summary(&outcome, None, None, ForkTail::NONE)
            .expect("the seed fork lands");
    }

    // The restart: this daemon composes the current prompt for the same seat.
    let serv =
        canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "re-seat noted")], 1);
    let mut now = seated.clone();
    now.endpoint = serv.endpoint.clone();
    let registry = Registry::new();
    registry
        .create(id, "a session", wiring(&now))
        .expect("the session is created");
    let mut sessions =
        Sessions::open_first(&parts, now, registry.clone()).expect("the daemon opens");
    assert!(
        registry.stale_prefix(id).is_some(),
        "the fixture must be a session speaking a stale prompt, or the re-seat proves nothing"
    );

    let out = sessions.dispatch(
        id,
        &QueuedCommand {
            head_id: DAEMON_SUBMITTER.into(),
            identity: "dead".into(),
            client_request_id: "reseat-1".into(),
            at_seq: 0,
            enqueued_ms: 0,
            kind: CommandKind::Reseat { summarise: false },
        },
    );
    assert!(
        matches!(out, Outcome::Compacted(_)),
        "the re-seat itself is reported as it always was"
    );
    assert_eq!(
        registry.stale_prefix(id),
        None,
        "a re-seated session is no longer stale"
    );

    let items = sessions
        .harness_of(id)
        .expect("the session is open")
        .items()
        .to_vec();
    let told = items
        .iter()
        .position(|i| match i {
            TranscriptItem::User {
                speaker: Speaker::Agent,
                parts,
            } => parts
                .iter()
                .any(|p| matches!(p, UserPart::Text { text } if text.starts_with("[reseat] "))),
            _ => false,
        })
        .unwrap_or_else(|| {
            panic!("the model was never told it was re-seated, so it goes on asking: {items:?}")
        });
    let TranscriptItem::User { parts, .. } = &items[told] else {
        unreachable!()
    };
    let text = parts
        .iter()
        .find_map(|p| match p {
            UserPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .expect("the row has text");
    assert!(
        text.contains("stop asking for `/reseat`"),
        "the notice it was sent told it to keep asking; this one has to tell it to stop: {text}"
    );
    assert!(
        items[told + 1..]
            .iter()
            .any(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains("re-seat noted"))),
        "the turn has to RUN — a row nobody answers is the silence this replaces: {items:?}"
    );

    // And the head was told too, as before.
    let hub = registry.get(id).expect("the hub");
    assert!(
        hub.retained().iter().any(|e| matches!(&e.event,
            SessionEvent::Warning { code, .. } if code == "reseated")),
        "the head's `reseated` row is still said"
    );
}
