//! The model is told its prompt is stale ONCE, and as information. The operator asked for
//! the reseat card to stay in front of them - the person owns the decision and can continue
//! at their own risk.

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{ForkTail, Harness};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};
use letibot_turn::CompactionOutcome;

mod support;

use support::canned;
use support::qwen::{IM_END, THINK_CLOSE, spoken};

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "harnessd-stale-once-{tag}-{}-{}",
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
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
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

/// Two plan-checks on a stale session: the first carries the sentence, the second does not.
#[test]
fn the_model_is_told_the_prompt_is_stale_once_and_not_on_every_check() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("store");
    let ws = TempDir::new("ws");
    let store = dir.0.join("sessions.db");
    let id = "s-stale-once";
    let seated = config(&store, id, &ws.0);
    let parts = Parts::load(&seated).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

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
            budget: None,
            fidelity: None,
        };
        h.fork_to_summary(&outcome, None, None, ForkTail::NONE)
            .expect("the seed fork lands");
    }

    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(&parts.vocab, "noted"),
            a_plain_answer(&parts.vocab, "still working"),
        ],
        2,
    );
    let mut now = seated.clone();
    now.system = "the prompt this daemon composes today".into();
    now.endpoint = serv.endpoint.clone();
    let hub = Hub::new(id);
    let registry = Registry::of(hub.clone());
    let mut h = Harness::open_with_registry(&parts, now, hub, None, None, registry.clone())
        .expect("the session reopens");
    let said = registry
        .stale_for_model(id)
        .expect("the fixture must be a session speaking a stale prompt, or this proves nothing");

    let first = h.nag_notice().expect("a stale prompt is something to say");
    assert!(
        first.contains(&said),
        "the first check carries the model's own sentence: {first}"
    );
    assert!(
        first.contains("[stale prompt]") && first.contains("the person's call"),
        "and says whose decision it is, instead of telling the model to keep asking: {first}"
    );
    // R93.E9: the model's copy names no session and cannot name `/reseat` - that is the head's
    // sentence, and the model can act on neither the id nor the verb.
    assert!(
        !first.contains(id) && !first.contains("/reseat"),
        "the operator's sentence is not the model's: {first}"
    );
    assert!(
        !first.contains("keep saying it"),
        "the old instruction to repeat it is gone: {first}"
    );
    h.nag_turn().expect("the first check runs");

    assert_eq!(
        h.nag_notice(),
        None,
        "told once: with no plan, the next check has nothing left to say"
    );
    assert!(h.nag_turn().expect("a check with nothing to say").is_none());
    assert!(
        registry.stale_prefix(id).is_some(),
        "the prompt is still stale — the head's card still says so; only the model stops"
    );

    let told = h
        .items()
        .iter()
        .filter(|i| match i {
            TranscriptItem::User {
                speaker: Speaker::Agent,
                parts,
            } => parts
                .iter()
                .any(|p| matches!(p, UserPart::Text { text } if text.contains(&said))),
            _ => false,
        })
        .count();
    assert_eq!(told, 1, "the sentence is in the conversation once");
}
