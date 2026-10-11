//! **A model that stops because "my context is full" when it is not gets told so** —
//! R71, through the harness's own round loop.
//!
//! The canned server the turn crate's tests replay plays the model: one round ends the
//! turn with leticl's sentence of 2026-10-11 11:48, verbatim — *"My context is at the
//! wall, so I'll stop here rather than start a piece I can't finish"* — and no tool
//! call. What is asserted is the whole of the check's contract with the loop:
//!
//! 1. at a LOW measured fill the harness appends its line — the harness's voice, never
//!    the operator's, with the numbers — and the loop continues, so the next round runs;
//! 2. at the WALL the claim is true, nothing is said, and the turn ends where the model
//!    ended it;
//! 3. a model that says it again after being told is the operator's business: a hub
//!    warning, not a second line to the model.
//!
//! It needs the vocabulary GGUF and not a model.

use letibot_harnessd::Parts;
use letibot_harnessd::config::Config;
use letibot_harnessd::context_claim::{LOG, REPEATED};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

// Shared by path with the turn crate, which uses the parts this file does not.
#[allow(dead_code)]
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Reply;

/// leticl, 2026-10-11 11:48, verbatim.
const EXCUSE: &str = "My context is at the wall, so I'll stop here rather than start a piece I can't finish: what's left of the port is scroll regions.";

const IM_END: u32 = 248046;
const THINK_CLOSE: u32 = 248069;

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
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

fn config(dir: &TempDir, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box(&dir.path);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(dir.path.join("sessions.db"));
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

/// A round that answers in plain text and makes no tool call — the shape that ends a
/// turn. `n_prompt` is what the provider reports the prompt cost, which is the other
/// half of the ratio the wall is judged in.
fn a_plain_answer(vocab: &letibot_tokencore::Vocab, answer: &str, n_prompt: u64) -> Reply {
    let thought = vocab.tokenize_text("thinking").expect("tokenizes");
    let answer_ids = vocab.tokenize_text(answer).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    Reply::Frames(frames)
}

/// The harness's context-check lines in the transcript, as the model will read them.
fn context_checks(h: &Harness) -> Vec<(letibot_transcript::Speaker, String)> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { speaker, parts } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } if text.starts_with("[context check]") => {
                    Some((*speaker, text.clone()))
                }
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

fn warnings_of(hub: &Hub, code: &str) -> Vec<String> {
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

/// **The leticl turn at a fill far from the wall: told, with the numbers, and the work
/// continues.**
#[test]
fn an_excuse_at_low_fill_is_answered_with_the_measured_fill_and_the_turn_continues() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-low");
    let mut cfg = config(&dir, "context-claim-low");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, 30),
            a_plain_answer(vocab, "Continuing with the scroll regions.", 30),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new("context-claim-low");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(reply.rounds, 2, "the line continued the turn: {reply:?}");
    assert!(
        reply.text.contains("Continuing with the scroll regions."),
        "the second round is the reply: {:?}",
        reply.text
    );
    let checks = context_checks(&h);
    assert_eq!(checks.len(), 1, "one line, once: {checks:#?}");
    let (speaker, line) = &checks[0];
    assert_eq!(
        *speaker,
        Speaker::Agent,
        "the harness speaking, never the operator"
    );
    assert!(
        line.starts_with("[context check] you said \"My context is at the wall"),
        "{line}"
    );
    assert!(line.contains("% full — "), "the numbers: {line}");
    assert!(line.contains(" tokens. That is not the wall"), "{line}");
    assert!(line.ends_with("Continue with what you stopped."), "{line}");
    assert!(warnings_of(&hub, REPEATED).is_empty());

    let log = std::fs::read_to_string(dir.path.join(LOG)).expect("the claim is logged");
    let row: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json");
    assert_eq!(row["slapped"], true, "{row}");
    assert_eq!(row["at_wall"], false, "{row}");
    // **The window as configured** (TODO R81): it used to be converted through the ratio the
    // round measured and land a token off — every row of the operator's file said `999999`.
    let window = row["window"].as_u64().expect("a measured window");
    assert_eq!(window, 1_000_000, "{row}");
}

/// **The same sentence AT the wall is true** — no line, and the turn ends where the
/// model ended it. The window is set to what the session already holds, and automatic
/// compaction is off so the round loop does not stop the turn for the wall itself.
#[test]
fn the_same_excuse_at_the_wall_is_left_alone() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-wall");
    let mut cfg = config(&dir, "context-claim-wall");
    cfg.auto_compact = false;
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;

    // The prefix this session opens with is what the window is set to, so the session
    // is at the wall before the turn starts. The provider's count is reported as the
    // same, so the measured ratio is ~1 and the window is not scaled away from it.
    let resident = {
        let probe = Harness::open(&parts, cfg.clone(), Hub::new("probe")).expect("opens");
        probe.ledger_len() as u64
    };
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, resident + 16),
            // A spare, so an unexpected second round is an assertion and not a socket
            // error.
            a_plain_answer(vocab, "Continuing.", resident + 16),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();
    cfg.session_id = "context-claim-wall-2".into();
    cfg.context_window = Some(resident);

    let hub = Hub::new("context-claim-wall-2");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(reply.rounds, 1, "nothing continued the turn: {reply:?}");
    assert!(
        context_checks(&h).is_empty(),
        "a true claim is not corrected"
    );
    let log = std::fs::read_to_string(dir.path.join(LOG)).expect("the claim is logged");
    let row: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json");
    assert_eq!(row["at_wall"], true, "{row}");
    assert_eq!(row["slapped"], false, "{row}");
}

/// **Said again after being told: the operator hears, the model is not told twice.**
#[test]
fn an_excuse_repeated_after_the_line_is_a_warning_for_the_operator() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-again");
    let mut cfg = config(&dir, "context-claim-again");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, 30),
            a_plain_answer(
                vocab,
                "I am at the end of my context, so this is where I stop.",
                30,
            ),
            a_plain_answer(vocab, "spare", 30),
        ],
        3,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new("context-claim-again");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(reply.rounds, 2, "told once, then the turn ends: {reply:?}");
    assert_eq!(
        context_checks(&h).len(),
        1,
        "one line to the model, not two"
    );
    let warned = warnings_of(&hub, REPEATED);
    assert_eq!(warned.len(), 1, "{warned:#?}");
    assert!(
        warned[0].contains("I am at the end of my context"),
        "{}",
        warned[0]
    );
}
