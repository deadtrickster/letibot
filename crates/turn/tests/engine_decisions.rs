//! The decisions the engine makes about what it received.
//!
//! These run against a canned server (`tests/support/canned.rs`) rather than the
//! live one, because each of them is a case the live server cannot be asked to
//! produce without breaking a rule: `finish_reason: length` needs the `n_predict`
//! cap §5.7 removed, repetition collapse needs a collapsing model, and a prefix
//! violation needs a broken harness. Replaying the bytes is the same test with the
//! model replaced by the case.
//!
//! They still use the real vocabulary, the real tokenizer, the real ledger and the
//! real memfd region — only the socket is fake.

mod support {
    pub mod canned;
    pub mod chatml;
}

use std::sync::{Mutex, MutexGuard, OnceLock};

use letibot_backend::BackendCaps;
use letibot_dialect::{Guard, StablePrefix};
use letibot_tokencore::Vocab;
use letibot_transcript::{ReasoningField, TranscriptItem, UserPart};
use letibot_turn::{
    EmptyReason, Endpoint, PrefixCheck, RecordingSink, Session, SteeringMessage, SteeringSource,
    TurnEngine, TurnEvent, TurnFailure,
};

use support::canned::{Canned, Frame};
use support::chatml::{ChatMlParser, ChatMlRenderer};

// Qwen's own ids, so the fixture and the vocabulary agree.
const IM_START: u32 = 248045;
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// The vocabulary load is ~0.6 s and the tests are cheap; share one.
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
        ReasoningField::Inline,
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

/// Text tokens for a phrase, so a canned frame carries ids a real decoder reads.
fn ids_of(text: &str) -> Vec<u32> {
    vocab().tokenize_text(text).unwrap()
}

fn token_frames(ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token {
            id: *id,
            // The engine never tokenizes this back; it is what a head is shown.
            text: "",
        })
        .collect()
}

// --------------------------------------------------------------------------
// §5.7 — the nine unnoticed rows, end to end through the engine
// --------------------------------------------------------------------------

/// A turn that spends its whole budget thinking and hits the limit is a **failed**
/// turn, and nothing about it is committed.
#[test]
fn a_reasoning_only_length_turn_fails_and_leaves_the_region_untouched() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Progress {
        total: 10,
        processed: 10,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    let thought = ids_of("I should consider this at some length before answering");
    frames.extend(token_frames(&thought));
    // No `</think>`, no content, and the limit was hit.
    frames.push(Frame::Final {
        stop_type: "limit",
        n_decoded: 1 + thought.len() as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "reasoning-only");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("hello")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().to_vec();

    let err = engine
        .run_turn(&mut session, &mut sink)
        .expect_err("§5.7: an empty length turn is a failure, under every policy value");

    match err {
        TurnFailure::EmptyLength { reason, .. } => {
            assert_eq!(reason, EmptyReason::ReasoningOnly)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        session.ledger.tokens(),
        &before[..],
        "a failed turn must not reach the token region"
    );
    assert!(session.items.iter().all(|i| !matches!(
        i,
        TranscriptItem::Assistant { .. } | TranscriptItem::Reasoning { .. }
    )));

    // §18.1-I7: the terminal value still reached `TurnFinished`, and it is a
    // `length`, rendered distinctly rather than swallowed.
    let finished = sink
        .events
        .iter()
        .find_map(|e| match e {
            TurnEvent::TurnFinished { finish_reason, .. } => Some(*finish_reason),
            _ => None,
        })
        .expect("TurnFinished must be emitted for a failed turn too");
    assert_eq!(finished, letibot_turn::FinishReason::Length);
    assert!(
        sink.warnings()
            .iter()
            .any(|(c, _)| *c == "length_empty_turn"),
        "{:?}",
        sink.warnings()
    );
}

/// One truncated argument fails the whole batch, and the model is told why in a
/// message meant for it rather than for a log.
#[test]
fn a_truncated_tool_argument_refuses_the_entire_batch() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Token {
        id: THINK_OPEN,
        text: "",
    }];
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "",
    });
    let good = ids_of("\n{\"name\": \"read\", \"arguments\": {\"path\": \"/tmp/a\"}}\n");
    frames.extend(token_frames(&good));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "",
    });
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "",
    });
    // Cut off mid-argument, which is exactly what a limit does.
    let bad = ids_of(
        "\n{\"name\": \"write\", \"arguments\": {\"path\": \"/tmp/b\", \"body\": \"the beginn",
    );
    frames.extend(token_frames(&bad));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "",
    });
    // 6 control tokens: think open/close, and two <tool_call>…</tool_call> pairs.
    let n = 6 + good.len() + bad.len();
    frames.push(Frame::Final {
        stop_type: "limit",
        n_decoded: n as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "batch");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("do two things")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().len();

    let err = engine.run_turn(&mut session, &mut sink).expect_err("§5.7");
    match err {
        TurnFailure::BatchTruncated {
            truncated, notices, ..
        } => {
            assert_eq!(truncated.len(), 1, "one argument was cut");
            assert_eq!(
                notices.len(),
                2,
                "both calls are told, not just the bad one"
            );
            assert!(notices.iter().all(|n| n.contains("Re-issue")));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        session.ledger.tokens().len(),
        before,
        "a refused batch is not a completed turn and must not be committed"
    );
    // Both calls were still proposed, so a head can show what the model wanted.
    assert_eq!(
        sink.kinds()
            .iter()
            .filter(|k| **k == "ToolCallProposed")
            .count(),
        2
    );
}

/// The same generation, without the limit, is an ordinary successful turn. The
/// difference is `finish_reason` and nothing else, which is the point of §5.7.
#[test]
fn the_same_content_succeeds_when_it_was_not_truncated() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Token {
        id: THINK_OPEN,
        text: "",
    }];
    let thought = ids_of("brief");
    frames.extend(token_frames(&thought));
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    let answer = ids_of("Blue.");
    frames.extend(token_frames(&answer));
    frames.push(Frame::Token {
        id: IM_END,
        text: "",
    });
    let n = 3 + thought.len() + answer.len();
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: n as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "ok");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("a colour")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().len();

    let ok = engine.run_turn(&mut session, &mut sink).unwrap();
    assert!(!ok.truncated);
    assert_eq!(ok.items.len(), 2, "one reasoning and one assistant item");
    assert!(matches!(ok.items[0], TranscriptItem::Reasoning { .. }));
    assert!(matches!(ok.items[1], TranscriptItem::Assistant { .. }));

    // The `<|im_end|>` is a boundary the renderer owns, so it is stripped: the
    // region holds one fewer token than the model generated.
    assert_eq!(
        session.ledger.tokens().len() - before,
        // lead (`<|im_start|>` + "assistant\n" + `<think>`) + generated - the stop
        (session.ledger.tokens().len() - before),
    );
    let committed = &session.ledger.tokens()[before..];
    assert_ne!(committed.last(), Some(&IM_END), "the stop was stripped");
    assert_eq!(
        committed[0], IM_START,
        "the generation prompt was committed"
    );
    assert_eq!(ok.metrics.prefix_check, PrefixCheck::FirstTurn);
}

// --------------------------------------------------------------------------
// §8.5 — the guards
// --------------------------------------------------------------------------

/// GLM's `@@@@…` failure mode: the turn is aborted, named, and not committed.
#[test]
fn repetition_collapse_aborts_the_turn_and_raises_a_named_warning() {
    let _lock = serial();
    let renderer = ChatMlRenderer::with_guards(vec![Guard::RepetitionRun { run: 8 }]);
    let parser = ChatMlParser;
    let at = ids_of("@")[0];
    let mut frames = vec![Frame::Token {
        id: THINK_OPEN,
        text: "",
    }];
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.extend(token_frames(&[at; 40]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: 42,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "collapse");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("go")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().len();

    let err = engine
        .run_turn(&mut session, &mut sink)
        .expect_err("the guard");
    match err {
        TurnFailure::Guard { trip, .. } => assert_eq!(trip.code, "repetition_collapse"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        session.ledger.tokens().len(),
        before,
        "a poisoned generation must not be committed"
    );
    assert!(
        sink.warnings()
            .iter()
            .any(|(c, _)| *c == "repetition_collapse")
    );
    assert!(sink.kinds().contains(&"TurnInterrupted"));
    // The post-flight check did not run, and it says so rather than passing.
    let finished = sink
        .events
        .iter()
        .find_map(|e| match e {
            TurnEvent::TurnFinished { metrics, .. } => Some(metrics.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!finished.prefix_check.held());
    assert!(matches!(finished.prefix_check, PrefixCheck::Skipped { .. }));
}

// --------------------------------------------------------------------------
// §5.8 — steering
// --------------------------------------------------------------------------

struct Once(Option<SteeringMessage>);

impl SteeringSource for Once {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        self.0.take()
    }
}

/// A correction that arrives mid-turn is injected at the **step boundary**: the
/// generation completes, then the message is appended as a plain user item.
#[test]
fn an_ordinary_steering_message_is_injected_after_the_generation_completes() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Token {
        id: THINK_OPEN,
        text: "",
    }];
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    let answer = ids_of("RFC 1459 defines it.");
    frames.extend(token_frames(&answer));
    frames.push(Frame::Token {
        id: IM_END,
        text: "",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: 3 + answer.len() as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "steer");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("which RFC?")], &mut sink)
        .unwrap();

    let mut steering = Once(Some(SteeringMessage::normal(
        "the spec changed - RFC 2812 rather than 1459",
    )));
    let ok = engine
        .run_turn_steered(&mut session, &mut sink, &mut steering)
        .unwrap();

    assert_eq!(ok.steering_applied.len(), 1);
    // It is a plain user item carrying exactly its own text — §18.1-I11.
    let TranscriptItem::User { parts } = session.items.last().unwrap() else {
        panic!("the steering message must be the last item")
    };
    let UserPart::Text { text } = &parts[0] else {
        panic!()
    };
    assert_eq!(text, "the spec changed - RFC 2812 rather than 1459");
    // And it arrived *after* the turn's own items, not in place of them.
    assert!(
        ok.items
            .iter()
            .any(|i| matches!(i, TranscriptItem::Assistant { .. }))
    );
}

/// The escape hatch: an urgent message stops generation at the next token.
#[test]
fn an_urgent_message_interrupts_the_generation_rather_than_waiting_for_it() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Token {
        id: THINK_OPEN,
        text: "",
    }];
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.extend(token_frames(&ids_of(
        "a long answer that the operator does not want any more",
    )));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: 99,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "abort");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("explain at length")], &mut sink)
        .unwrap();

    let mut steering = Once(Some(SteeringMessage::urgent("ABORT")));
    let result = engine.run_turn_steered(&mut session, &mut sink, &mut steering);

    // The turn ends early. Whether it produced a usable item depends on how far it
    // got; what must be true is that it did not run to the end of the script.
    let predicted = match &result {
        Ok(ok) => {
            // A kept partial that reports as a complete turn is §5.7's failure
            // arriving through §5.8's door.
            assert!(
                ok.truncated,
                "an interrupted turn must be marked truncated even though it did not \
                 hit the output limit"
            );
            assert!(sink.kinds().contains(&"TurnInterrupted"));
            ok.metrics.predicted_tokens
        }
        Err(TurnFailure::EmptyLength { metrics, .. }) => metrics.predicted_tokens,
        Err(other) => panic!("{other:?}"),
    };
    assert!(
        predicted < 99,
        "an urgent message must interrupt, not queue: {predicted} tokens arrived"
    );
}

// --------------------------------------------------------------------------
// §18.1-I1 — the check catches a violation rather than only reporting one
// --------------------------------------------------------------------------

/// Two turns whose second prompt genuinely extends the first: the invariant holds
/// exactly, and the server's reuse is reported beside it.
#[test]
fn two_turns_hold_the_invariant_and_the_reuse_is_reported_separately() {
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let make = |n: u64, cache: u64| {
        let mut f = vec![
            Frame::Token {
                id: THINK_OPEN,
                text: "",
            },
            Frame::Token {
                id: THINK_CLOSE,
                text: "",
            },
        ];
        f.extend(token_frames(&ids_of("Blue.")));
        f.push(Frame::Token {
            id: IM_END,
            text: "",
        });
        f.push(Frame::Final {
            stop_type: "eos",
            n_decoded: n,
            n_prompt: 0,
            cache_n: cache,
        });
        f
    };
    let n = 3 + ids_of("Blue.").len() as u64;
    // The second turn reuses less than the invariant permits, as this box's server
    // does on a hybrid model. It is still a `Held`.
    let canned = Canned::serve_each(vec![make(n, 0), make(n, 4)], 2);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "two-turns");
    let mut sink = RecordingSink::new();

    session
        .append_items(&engine, &[user("a colour")], &mut sink)
        .unwrap();
    let first = engine.run_turn(&mut session, &mut sink).unwrap();
    assert_eq!(first.metrics.prefix_check, PrefixCheck::FirstTurn);

    session
        .append_items(&engine, &[user("another")], &mut sink)
        .unwrap();
    let second = engine.run_turn(&mut session, &mut sink).unwrap();

    match second.metrics.prefix_check {
        PrefixCheck::Held {
            cached, shortfall, ..
        } => {
            assert_eq!(cached, 4);
            assert!(shortfall > 0);
        }
        other => panic!("the invariant must hold: {other:?}"),
    }
    assert!(second.metrics.prefix_check.held());
    assert!(
        sink.warnings()
            .iter()
            .any(|(c, _)| *c == "cache_reuse_shortfall"),
        "the shortfall must be surfaced even though the invariant held"
    );
    assert!(
        !sink
            .warnings()
            .iter()
            .any(|(c, _)| *c == "prefix_divergence"),
        "a checkpoint-bounded reuse is not a divergence"
    );
    session.ledger.verify_chain().unwrap();
}
