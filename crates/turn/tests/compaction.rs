//! The compaction turn, against the canned server (C1).
//!
//! The strategy under test — re-send what the previous turn sent and append the
//! instruction as one more message — is not visible in a single turn's frames,
//! because it is a property of the *prompt*: the ledger region the engine
//! submits. What a canned test can assert is that the operation appends the
//! instruction, runs one normal turn, reads the summary out of it, and
//! surfaces the reuse numbers and any tool calls instead of swallowing them.
//! The reuse itself is the prefix invariant's job and is asserted there.

mod support {
    pub mod canned;
    pub mod chatml;
}

use std::sync::{Mutex, MutexGuard, OnceLock};

use letibot_backend::BackendCaps;
use letibot_dialect::StablePrefix;
use letibot_tokencore::Vocab;
use letibot_transcript::{SystemOrigin, TranscriptItem, UserPart};
use letibot_turn::{Endpoint, RecordingSink, Session, TurnEngine, TurnFailure};

use support::canned::{Canned, Frame};
use support::chatml::{ChatMlParser, ChatMlRenderer};

// Qwen's own ids, so the fixture and the vocabulary agree (same values the
// engine-decision tests pin).
const IM_END: u32 = 248046;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

fn vocab() -> &'static Vocab {
    static VOCAB: OnceLock<Vocab> = OnceLock::new();
    VOCAB.get_or_init(|| {
        let path = std::env::var("LETIBOT_VOCAB_GGUF").unwrap_or_else(|_| {
            "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf"
                .to_string()
        });
        let p = std::path::PathBuf::from(&path);
        assert!(p.is_file(), "no vocabulary GGUF at {path}");
        Vocab::load(&p).expect("the vocabulary must load")
    })
}

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn engine<'a>(
    renderer: &'a ChatMlRenderer,
    parser: &'a ChatMlParser,
    endpoint: Endpoint,
) -> TurnEngine<'a> {
    TurnEngine::new(
        vocab(),
        renderer,
        parser,
        endpoint,
        BackendCaps::OWN_SERVER,
        "canned",
        serde_json::json!({}),
    )
    .expect("the fixture dialect resolves against Qwen's vocabulary")
}

fn session(engine: &TurnEngine<'_>, tag: &str) -> Session {
    engine
        .open(
            tag,
            &StablePrefix {
                system: "be terse".into(),
                tools_json: vec![],
            },
        )
        .unwrap()
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn ids_of(text: &str) -> Vec<u32> {
    vocab().tokenize_text(text).unwrap()
}

fn spoken_frames(ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token {
            id: *id,
            text: vocab()
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

/// A complete turn whose answer is `answer`: the lead has opened `<think>`, the
/// model thinks, closes, answers, ends.
fn a_summary_turn(thought: &[u32], answer: &[u32], n_prompt: u64, cache_n: u64) -> Vec<Frame> {
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken_frames(thought));
    frames.extend(spoken_frames(&[THINK_CLOSE]));
    frames.extend(spoken_frames(answer));
    frames.extend(spoken_frames(&[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer.len() + 2) as u64,
        n_prompt,
        cache_n,
    });
    frames
}

/// Two ordinary turns, then the compaction turn. The assertions:
///
/// * the instruction went into the ledger as a `System` update, after the
///   history, distinguishable from anything anybody typed;
/// * the summary is the turn's visible text, verbatim;
/// * the reuse numbers come off the turn's own metrics — `reusable` is what
///   the prefix invariant permitted (the previous turn's prompt plus its
///   committed generation) and `cached_tokens` is what the server reported,
///   with no reconciliation between them.
#[test]
fn compaction_appends_the_instruction_and_reads_the_summary_off_the_turn() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);

    // Turn one: an ordinary answer over a session with one user item.
    let answer = ids_of("the answer was forty-two");
    let first = a_summary_turn(&ids_of("count the things"), &answer, 20, 18);
    let canned = Canned::serve(first, 1);
    let mut engine1 = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine1, "compact-happy");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine1, &[user("count the things")], &mut sink)
        .unwrap();
    let ok = engine1.run_turn(&mut session, &mut sink).unwrap();
    let witness = session.witness().expect("a completed turn left a witness").clone();
    let _ = ok;

    // The compaction turn, served over the same session.
    let summary_text = "decided: answer is forty-two; no files changed; open: none";
    let frames = a_summary_turn(
        &ids_of("gather the record"),
        &ids_of(summary_text),
        40,
        39, // the server reused all but the instruction suffix
    );
    let canned = Canned::serve(frames, 1);
    let mut engine2 = engine(&renderer, &parser, canned.endpoint.clone());
    let outcome = letibot_turn::run_compaction(&mut engine2, &mut session, &mut sink).unwrap();

    assert_eq!(outcome.summary, summary_text, "{:?}", outcome.summary);
    assert_eq!(outcome.tool_calls, 0);
    // What the invariant permitted: the first turn's prompt plus its committed
    // generation — proven identical over that span, whatever the server did.
    assert_eq!(outcome.reusable, witness.covered_len as u64);
    assert_eq!(outcome.cached_tokens, 39);
    assert!(outcome.generated_tokens > 0);

    // The ledger carries the instruction as a system update — distinguishable
    // from anything anybody typed — and the turn's own items after it: the
    // reasoning, then the summary as the final assistant text.
    let instruction_at = session
        .items
        .iter()
        .position(|i| {
            matches!(i, TranscriptItem::System { origin: SystemOrigin::Update, .. })
                && match i {
                    TranscriptItem::System { text, .. } => {
                        text == letibot_turn::SUMMARY_INSTRUCTION
                    }
                    _ => false,
                }
        })
        .expect("the instruction is in the ledger as a system update");
    assert!(
        session.items[..instruction_at]
            .iter()
            .all(|i| !matches!(i, TranscriptItem::System { .. })),
        "nothing system-typed came before it"
    );
    assert_eq!(
        outcome.summary,
        match session.items.last().unwrap() {
            TranscriptItem::Assistant { text, .. } => text.as_str(),
            other => panic!("{other:?}"),
        }
    );
}

/// A summary turn that starts working instead of answering is surfaced, not
/// swallowed: the outcome carries the tool calls and the caller refuses.
#[test]
fn a_summary_turn_that_calls_tools_surfaces_them() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);

    let mut frames = vec![Frame::Progress {
        total: 30,
        processed: 30,
    }];
    frames.extend(spoken_frames(&[THINK_CLOSE]));
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "",
    });
    let call = ids_of("\n{\"name\": \"read\", \"arguments\": {\"path\": \"/tmp/a\"}}\n");
    frames.extend(call.iter().map(|id| Frame::Token { id: *id, text: "" }));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "",
    });
    frames.extend(spoken_frames(&[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (call.len() + 4) as u64,
        n_prompt: 30,
        cache_n: 29,
    });
    let canned = Canned::serve(frames, 1);
    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "compact-tools");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("summarise")], &mut sink)
        .unwrap();

    let outcome = letibot_turn::run_compaction(&mut engine, &mut session, &mut sink).unwrap();
    assert_eq!(outcome.tool_calls, 1, "{outcome:?}");
    assert!(outcome.summary.is_empty(), "{:?}", outcome.summary);
}

/// A summary turn that fails under §5.7 fails the compaction, and nothing is
/// reduced: the instruction stays in the ledger, which is where the next
/// attempt appends over it.
#[test]
fn a_failed_summary_turn_fails_the_compaction_and_reduces_nothing() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);

    // All budget spent thinking; `finish_reason: length`, no content.
    let mut frames = vec![Frame::Progress {
        total: 30,
        processed: 30,
    }];
    let thought = ids_of("thinking about the summary at length without ever closing");
    frames.extend(spoken_frames(&thought));
    frames.push(Frame::Final {
        stop_type: "limit",
        n_decoded: thought.len() as u64,
        n_prompt: 30,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);
    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "compact-fail");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("summarise")], &mut sink)
        .unwrap();
    let before_compaction = session.ledger.tokens().to_vec();

    let err = letibot_turn::run_compaction(&mut engine, &mut session, &mut sink).unwrap_err();
    assert!(matches!(err, TurnFailure::EmptyLength { .. }), "{err:?}");
    // The failed turn committed nothing: the region grew by exactly the
    // instruction, and no assistant or reasoning item exists anywhere.
    assert!(session.ledger.tokens().len() > before_compaction.len());
    assert!(session.items.iter().all(|i| !matches!(
        i,
        TranscriptItem::Assistant { .. } | TranscriptItem::Reasoning { .. }
    )));
    assert!(matches!(
        session.items.last().unwrap(),
        TranscriptItem::System { origin: SystemOrigin::Update, .. }
    ));
}
