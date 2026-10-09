//! **The wall as the provider speaks it**: a context-length refusal is the
//! wall, and every door that starts a turn recovers from it.
//!
//! The wedge this file is the regression guard for was measured 2026-10-09 on
//! `s-1789919514688401228` (pg-noop) on `deepseek/deepseek-flash`:
//!
//! * the daemon said `context 1000000 tokens; compacts automatically with
//!   62500 left`, so the operator believed the wall was managed;
//! * ONE `compacting:` line in the whole log (`grep -c` → 1), after which the
//!   no-progress guard switched automatic compaction off in memory;
//! * the next thirty requests went out at 1,048,624 / 1,049,070 / 1,049,191
//!   tokens against the provider's 1,048,576 and came back
//!   `http 400: This model's maximum context length is …` — the FIRST
//!   attributed to `monitor` (an automatic wake), the LAST to `todo check`
//!   (the plan nag), and none of the thirty recovered, because the refusal
//!   landed as a backend error and every recovery was gated on
//!   `HarnessError::ContextWall`.
//!
//! Every fixture here replays the two measured refusal bodies VERBATIM (the
//! plain-text OpenAI shape deepseek answered with, and llama.cpp's
//! `exceed_context_size_error` JSON), because a matcher is only as narrow as
//! its fixtures are real. It needs the vocabulary GGUF and not a model: the
//! refusal is bytes on the wire, and the compaction's summary turns are
//! answered by the canned server the turn crate's tests replay.

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{Harness, HarnessError};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;

/// The canned server the turn crate's tests replay, shared by path rather than
/// copied — a second copy of the wire shape would drift from the first.
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Reply;

const WANTED: Dialect = Dialect::Qwen;

/// **The measured deepseek body, verbatim** — an OpenAI-shaped provider, plain
/// text, the numbers in the sentence and a request id trailing. The request id
/// is kept because a matcher that stopped at `Please reduce the length` would
/// be matching a sentence, and this body is the sentence the operator read.
const OPENAI_REFUSAL: &str = "This model's maximum context length is 1048576 tokens. However, you requested 1463497 tokens (1463497 in the messages, 0 in the completion). Please reduce the length of the messages or completion. (request_id: 8a448ce7-fabf-4ffa-b350-16e6ddeed6ac)";

/// **The measured llama.cpp body, verbatim** — JSON with a type field, the
/// numbers as fields. This is the structured half of the matcher's diet.
const LLAMACPP_REFUSAL: &str = r#"{"error":{"code":400,"message":"request (1504198 tokens) exceeds the available context size (262144 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":1504198,"n_ctx":262144}}"#;

/// A 400 that is NOT about context length — also measured, in the same log —
/// and the shape the matcher must refuse to read as the wall.
const INVALID_TOKENS: &str = r#"{"error":{"code":400,"message":"Prompt contains invalid tokens","type":"invalid_request_error"}}"#;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    // No retry ladder: a refusal is deterministic in the bytes, and waiting
    // 1+2+4+8+16+32 seconds to be told so again is the suite's time, not the
    // test's question.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

/// A directory that removes itself, because a test that leaks a store per run
/// eventually fills the disk and the run that finds out is not this one.
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

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// Qwen's own ids, the same values the turn crate's canned tests pin.
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

fn ids(vocab: &letibot_tokencore::Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
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

/// A complete turn that answers in plain text: the model thinks, closes the
/// block, answers, ends. No tool calls, so the round loop ends after one round.
fn a_plain_answer_turn(
    vocab: &letibot_tokencore::Vocab,
    thought: &str,
    answer: &str,
    n_prompt: u64,
) -> Vec<canned::Frame> {
    let thought_ids = ids(vocab, thought);
    let answer_ids = ids(vocab, answer);
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// The warnings a session's log holds, code and detail, in order — the order
/// is half of every assertion below (announced before attempted before done).
fn warnings_of(hub: &Hub) -> Vec<(String, String)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

/// **A 400 that is not about context length stays a backend error.**
///
/// The matcher's narrowness is the whole of its safety: a quota refusal, a
/// bad token, a wrong model name — each is a 400 with a body, and any of them
/// misread as the wall would put a session through a compaction it did not
/// need on top of a failure it did not deserve. This is the measured
/// invalid-tokens body from the same log.
#[test]
fn a_non_context_400_stays_a_backend_error() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-other");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-other-test";
    let mut cfg = config(&path, session_id);
    cfg.context_window = Some(32768);

    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;

    let scripts = vec![
        Reply::Status {
            code: 400,
            body: INVALID_TOKENS.into(),
        },
        // A spare answer, in case the shape of the failure ever changes enough
        // to spend a second request; an undersupplied server is a socket error
        // and a pass for the wrong reason.
        Reply::Frames(a_plain_answer_turn(vocab, "thinking", "answered", 30)),
    ];
    let serv = canned::Canned::serve_replies(scripts, 2);
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session opens");
    let out = h.submit("anything at all");
    match out {
        Err(HarnessError::ContextWall { .. }) => panic!(
            "an invalid-tokens 400 is a backend error, not the wall — the matcher is too wide"
        ),
        Err(e) => assert!(
            e.to_string().contains("http 400"),
            "the failure is the provider's own status and body: {e}"
        ),
        Ok(_) => panic!("a refused request must not come back Ok"),
    }
    // And nothing compacted behind it: a compaction here would be the
    // misdiagnosis spending a summary turn.
    let warnings = warnings_of(&hub);
    assert!(
        !warnings.iter().any(|(c, _)| c == "compacted"),
        "no compaction ran behind a non-context refusal: {warnings:#?}"
    );
}
