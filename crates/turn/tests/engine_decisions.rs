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
use letibot_transcript::{TranscriptItem, UserPart};
use letibot_turn::{
    DeltaTarget, EmptyReason, Endpoint, PrefixCheck, RecordingSink, Session, SteeringMessage,
    SteeringSource, TurnEngine, TurnEvent, TurnFailure,
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
        // One home for this path: `letibot_tokencore::apparatus`. It was
        // written out in seven crates, and `LETIBOT_VOCAB_GGUF` now wins
        // unconditionally there rather than being a hint.
        let p = letibot_tokencore::apparatus::gguf_path();
        assert!(p.is_file(), "no vocabulary GGUF at {}", p.display());
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
        speaker: Default::default(),
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

/// The same, but each frame carries the token's **own** text, the way the server
/// does.
///
/// `token_frames` sends `text: ""` because most cases here are about ids and a head
/// is not the subject. The channel tests below compare what a head was *shown*
/// against what the parser *committed*, so the two have to be the same bytes coming
/// out of the same vocabulary — including `</think>`, which Qwen marks
/// `USER_DEFINED` and which therefore does arrive in `content`. That is what made
/// it visible on a real head.
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

/// Every `Delta` a turn emitted, in order, with the channel it was announced on.
fn deltas(sink: &RecordingSink) -> Vec<(DeltaTarget, &str)> {
    sink.events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Delta { target, text, .. } => Some((*target, text.as_str())),
            _ => None,
        })
        .collect()
}

/// A turn shaped exactly like the live one T12 was measured on: the generation
/// prompt has already opened `<think>`, the model thinks, closes the block, answers
/// and ends the turn.
fn a_thinking_turn(thought: &[u32], answer: &[u32]) -> Vec<Frame> {
    let mut frames = vec![Frame::Progress {
        total: 12,
        processed: 12,
    }];
    frames.extend(spoken_frames(thought));
    frames.extend(spoken_frames(&[THINK_CLOSE]));
    frames.extend(spoken_frames(answer));
    frames.extend(spoken_frames(&[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer.len() + 2) as u64,
        n_prompt: 12,
        cache_n: 0,
    });
    frames
}

// --------------------------------------------------------------------------
// §5.7 — the nine unnoticed rows, end to end through the engine
// --------------------------------------------------------------------------

/// A turn that spends its whole budget thinking and hits the limit is a **failed**
/// turn, and nothing about it is committed.
#[test]
fn a_reasoning_only_length_turn_fails_and_leaves_the_region_untouched() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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

/// R7, the live finding replayed: the stream stops with `eos` — a *normal* stop,
/// which is the point — while the reasoning block is still open, and nothing but
/// reasoning was produced. The length classifier has no opinion on a `stop`, so
/// before the R7 check this turn committed as an ordinary empty one; the canned
/// bytes are msg_08d19c747001d1xPcOJgtgH10c's shape, seven tokens of the same
/// conflation at full size.
#[test]
fn a_turn_that_stops_inside_its_own_reasoning_is_not_an_empty_success() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Progress {
        total: 10,
        processed: 10,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    let thought = ids_of("mid-parenthetical, the think block never closed, so no");
    frames.extend(token_frames(&thought));
    // No `</think>` — and the stop is `eos`, not `limit`. That is the whole
    // difference from the test above, and it is what made the live finding
    // invisible to §5.7: the classifier only speaks on `length`.
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: 1 + thought.len() as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "unfinished-reasoning");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("hello")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().to_vec();

    let err = engine
        .run_turn(&mut session, &mut sink)
        .expect_err("a turn that ends inside its own reasoning is a failure, not an empty success");

    match err {
        TurnFailure::UnfinishedReasoning { turn_id, .. } => {
            assert!(turn_id.contains("unfinished-reasoning"), "{turn_id}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        session.ledger.tokens(),
        &before[..],
        "nothing was committed: the region is where it was"
    );
    assert!(session.items.iter().all(|i| !matches!(
        i,
        TranscriptItem::Assistant { .. } | TranscriptItem::Reasoning { .. }
    )));

    // The check names itself: the warning is the disclosure a log reader gets,
    // and `TurnFinished` still fires — the terminal value is a `stop`, rendered
    // as what it was, with the failure carried by the outcome, not the reason.
    assert!(
        sink.warnings()
            .iter()
            .any(|(c, _)| *c == "ended_in_reasoning"),
        "{:?}",
        sink.warnings()
    );
    let finished = sink
        .events
        .iter()
        .find_map(|e| match e {
            TurnEvent::TurnFinished { finish_reason, .. } => Some(*finish_reason),
            _ => None,
        })
        .expect("TurnFinished must be emitted for a failed turn too");
    assert_eq!(finished, letibot_turn::FinishReason::Eos);
}

/// The mirror case, so the check is not a tripwire on every thoughtful turn: the
/// block **closes**, content follows, and the same stop is an ordinary success.
#[test]
fn a_turn_that_closes_its_reasoning_and_says_something_still_succeeds() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Progress {
        total: 10,
        processed: 10,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    let thought = ids_of("brief thought");
    frames.extend(token_frames(&thought));
    frames.extend(token_frames(&[THINK_CLOSE]));
    let answer = ids_of("Here is the answer.");
    frames.extend(token_frames(&answer));
    frames.extend(token_frames(&[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (1 + thought.len() + 1 + answer.len() + 1) as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "closed-reasoning");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("hello")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().to_vec();

    engine
        .run_turn(&mut session, &mut sink)
        .expect("a closed reasoning block with content is an ordinary turn");
    assert_ne!(session.ledger.tokens(), &before[..], "the turn committed");
}

/// One truncated argument fails the whole batch, and the model is told why in a
/// message meant for it rather than for a log.
#[test]
fn a_truncated_tool_argument_refuses_the_entire_batch() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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

struct Once(Vec<SteeringMessage>);

impl SteeringSource for Once {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.remove(0))
        }
    }
    fn give_back(&mut self, msgs: Vec<SteeringMessage>) {
        // A handed-back message goes to the front, so the next `try_next` finds it
        // first: it was taken before anything still in the vector.
        self.0.splice(0..0, msgs);
    }
}

/// **A message already waiting when the turn begins goes in the prompt, not behind
/// it.**
///
/// The poll used to live only in the stream loop and at the boundary under it, so
/// the window where nobody listened was exactly "a tool is running" — and it cost
/// a round on top: the next generation was built and sent without the operator's
/// words, ran deaf, and appended them afterwards. Measured on the operator's own
/// session, 2026-09-20: a line typed during a three-minute `job_wait` sat queued
/// through that wait, through the next round, and through a 300-second permission
/// ask. *"basically it should be a little bit greedy with my messages"*.
///
/// `Once` is the fixture on purpose — it answers the very first poll, which is
/// what "was already waiting" means — and the two assertions are the two halves:
/// the message is reported as `steering_before`, and it is in the transcript
/// ahead of anything the model said this turn.
#[test]
fn a_message_already_waiting_is_read_before_the_model_speaks() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![
        Frame::Token {
            id: THINK_OPEN,
            text: "",
        },
        Frame::Token {
            id: THINK_CLOSE,
            text: "",
        },
    ];
    let answer = ids_of("RFC 2812 then.");
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
    let mut session = session(&engine, "greedy");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("which RFC?")], &mut sink)
        .unwrap();
    let before_len = session.items.len();

    let mut steering = Once(vec![SteeringMessage::normal("use 2812, not 1459")]);
    let ok = engine
        .run_turn_steered(&mut session, &mut sink, &mut steering)
        .unwrap();

    assert_eq!(
        ok.steering_before.len(),
        1,
        "it was taken before the generation: {:?}",
        ok.steering_before
    );
    assert!(
        ok.steering_applied.is_empty(),
        "and not again at the boundary: {:?}",
        ok.steering_applied
    );
    // The row sits where it was read: immediately after what was already there,
    // and ahead of the model's own items. That ordering is what `steering_before`
    // exists to let the caller reconcile.
    let TranscriptItem::User { parts, .. } = &session.items[before_len] else {
        panic!(
            "expected the steering row at {before_len}: {:?}",
            session.items
        )
    };
    let UserPart::Text { text } = &parts[0] else {
        panic!()
    };
    assert_eq!(text, "use 2812, not 1459");
    let assistant_at = session
        .items
        .iter()
        .position(|i| matches!(i, TranscriptItem::Assistant { .. }))
        .expect("the model answered");
    assert!(
        assistant_at > before_len,
        "the model spoke after reading it, not before"
    );
}

/// A correction that arrives mid-turn is injected at the **step boundary**: the
/// generation completes, then the message is appended as a plain user item.
///
/// The source is `After`, not `Once`, and that is the premise rather than a
/// detail. A turn now polls once BEFORE it builds its prompt — the greedy poll —
/// so a `Once` here would be consumed there and this test would silently measure
/// the other path under this name. `After` makes "arrives while the model is
/// speaking" a fact about the fixture instead of an accident of call order.
#[test]
fn an_ordinary_steering_message_is_injected_after_the_generation_completes() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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

    let mut steering = After {
        left: 2,
        msgs: vec![SteeringMessage::normal(
            "the spec changed - RFC 2812 rather than 1459",
        )],
    };
    let ok = engine
        .run_turn_steered(&mut session, &mut sink, &mut steering)
        .unwrap();

    assert!(
        ok.steering_before.is_empty(),
        "nothing was waiting when this turn began: {:?}",
        ok.steering_before
    );
    assert_eq!(ok.steering_applied.len(), 1);
    // It is a plain user item carrying exactly its own text — §18.1-I11.
    let TranscriptItem::User { parts, .. } = session.items.last().unwrap() else {
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

/// **A failed turn gives the operator's words back to the source, and commits
/// nothing.**
///
/// MEASURED on the operator's own head: they typed a prompt while a turn was
/// running, the daemon accepted it, and the round then failed under §5.7
/// (`length_empty_turn`). The words were out of the hub's queue the moment
/// `absorb` took them, and the failed turn committed nothing — so the words were
/// lost: not in the transcript, not in the source, and the head drew `queued` for
/// the rest of the session. This test holds the property that the words go back
/// to the source rather than being appended, so the next round's prompt contains
/// them.
///
/// The backend is the §5.7 fixture — reasoning only, at the limit — and the
/// steering is `After`, so the words arrive mid-generation rather than before it.
#[test]
fn a_failed_turn_gives_the_operators_words_back_to_the_source() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    // No close-think, no content, and the limit was hit: `length_empty_turn`.
    frames.push(Frame::Final {
        stop_type: "limit",
        n_decoded: 1 + thought.len() as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "steer-loss");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("hello")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().to_vec();

    // The operator's words, arriving mid-generation: the first two polls are
    // silent, and the third (the second frame) takes them.
    let mut steering = After {
        left: 2,
        msgs: vec![SteeringMessage::operator(
            "use web_search and fetch to lookup git commands meaning",
        )],
    };
    let err = engine
        .run_turn_steered(&mut session, &mut sink, &mut steering)
        .expect_err("§5.7: an empty length turn is a failure");

    match err {
        TurnFailure::EmptyLength { reason, .. } => {
            assert_eq!(reason, EmptyReason::ReasoningOnly)
        }
        other => panic!("{other:?}"),
    }

    // **The words are back in the source, not lost.** The failed turn committed
    // nothing, so the operator's words — which are input, not output — go back to
    // the source rather than being appended. The `left` counter is a test artifact
    // (it delays the words to mid-generation); the next round starts fresh, so
    // reset it and check that the words come out, with their flags intact.
    steering.left = 0;
    let back = steering
        .try_next()
        .expect("the operator's words are back in the source");
    assert_eq!(
        back.text,
        "use web_search and fetch to lookup git commands meaning"
    );
    assert!(back.from_operator, "still the operator's words");

    // And the failed turn committed no items: the ledger is exactly where it was,
    // and no assistant or reasoning item was appended.
    assert_eq!(
        session.ledger.tokens(),
        &before[..],
        "a failed turn must not reach the token region"
    );
    assert!(session.items.iter().all(|i| !matches!(
        i,
        TranscriptItem::Assistant { .. } | TranscriptItem::Reasoning { .. }
    )));
}

/// The escape hatch: an urgent message stops generation at the next token.
#[test]
fn an_urgent_message_interrupts_the_generation_rather_than_waiting_for_it() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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

    let mut steering = Once(vec![SteeringMessage::urgent("ABORT")]);
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    // **Measured, carried, and NOT put in front of the operator.** The shortfall
    // is still in `metrics.prefix_check` above — the numbers travel and a caller
    // that wants them has them — but it does not raise a warning, because the
    // invariant held and there is nothing to do about it.
    //
    // The operator's rule, 2026-09-15: "if problem wasnt us and restorable, why
    // it shown to me at all and not behind --debug in some log file?" The cost of
    // an unactionable warning is not its line, it is that it trains people to
    // skim the warnings that ARE actionable — `prefix_divergence` below being the
    // one that matters. `LETIBOT_DEBUG=1` brings it back.
    assert!(
        second.metrics.prefix_check.diagnostic_only(),
        "a held invariant is a diagnostic, not a warning"
    );
    assert!(
        !sink
            .warnings()
            .iter()
            .any(|(c, _)| *c == "cache_reuse_shortfall"),
        "an unactionable verdict must not compete with the actionable ones"
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

// --------------------------------------------------------------------------
// T12 — the live view and the stored view must agree about the channel
// --------------------------------------------------------------------------
//
// The symptom was one thing and the causes were two, so the cases below are
// written so that **each cause fails one of them on its own**. A single test that
// only goes red when both are broken would have gone green again on half a fix.
//
// `crates/sessionlog/tests/live_e2e.rs` asserts the same agreement end to end
// against the real model and a real head. These are here because a canned stream
// can hold the seed and the emission order still while the other one moves, which
// a live model cannot be asked to do.

/// **Cause 1, alone.** The generation prompt ends inside `<think>`, so the very
/// first generated token is reasoning and a head must be told so.
///
/// Nothing in this test involves a boundary token: it looks only at the deltas that
/// arrive *before* `</think>`. Reordering the emission back in front of the role
/// inspection does not change what it asserts, so a failure here means the seed —
/// and only the seed.
#[test]
fn the_first_delta_of_a_turn_is_reasoning_because_the_generation_prompt_opened_it() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let thought = ids_of("The user wants the days of the week.");
    let answer = ids_of("Monday");
    let canned = Canned::serve(a_thinking_turn(&thought, &answer), 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "seed");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("the days, please")], &mut sink)
        .unwrap();
    engine.run_turn(&mut session, &mut sink).unwrap();

    let deltas = deltas(&sink);
    let (target, text) = *deltas.first().expect("the turn streamed something");
    assert_eq!(
        target,
        DeltaTarget::Reasoning,
        "the first delta of the turn was announced as {target:?} carrying {text:?}. \
         The dialect's generation prompt ends in `<think>`, so the model was \
         reasoning before it emitted anything; seeding `in_reasoning` from a \
         *generated* `ThinkOpen` tells every head the reasoning is the answer for \
         the length of the turn."
    );
    // Every delta before the boundary, not merely the first: one per generated
    // token, all of them reasoning, and none of them left over for `Text`.
    assert_eq!(
        deltas
            .iter()
            .take_while(|(t, _)| *t == DeltaTarget::Reasoning)
            .count(),
        thought.len(),
        "the run of Reasoning deltas does not cover the whole thought: {deltas:?}"
    );
}

/// **Cause 2, alone.** A chunk's text must be cut against the roles of the ids in
/// it, so a boundary switches the channel for the rest of that same chunk and its
/// own literal is never streamed.
///
/// The seed is irrelevant to what this asserts: `</think>` is dropped and the
/// answer is `Text` whichever channel the turn started on, so a failure here means
/// the emission order — and only the emission order.
#[test]
fn a_boundary_literal_is_cut_out_of_the_stream_rather_than_shown_to_a_head() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let thought = ids_of("Seven of them.");
    let answer = ids_of("Monday");
    let canned = Canned::serve(a_thinking_turn(&thought, &answer), 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "boundary");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("the days, please")], &mut sink)
        .unwrap();
    engine.run_turn(&mut session, &mut sink).unwrap();

    let deltas = deltas(&sink);
    // The server really does send `</think>` in `content` — it is `USER_DEFINED`,
    // not `CONTROL` — and `spoken_frames` reproduces that. The parser drops it from
    // every committed row, so a head that renders it is showing markup the
    // transcript does not contain.
    for (target, text) in &deltas {
        assert!(
            !text.contains("</think>") && !text.contains("<|im_end|>"),
            "a boundary literal reached a head as {target:?} text: {text:?}. The \
             chunk's text was emitted before the ids in it had their roles \
             inspected."
        );
    }
    // And the switch happened at the boundary rather than one chunk late.
    assert_eq!(
        deltas.last().map(|(t, _)| *t),
        Some(DeltaTarget::Text),
        "the tokens after `</think>` are the answer and must be announced as Text"
    );
}

/// The agreement itself, deterministically: a head's live stream, split by channel,
/// **equals** the transcript rows the same turn committed.
///
/// This is the live file's assertion with the model replaced by a fixed script. It
/// fails under either cause, which is why the two above exist.
#[test]
fn a_heads_deltas_split_by_channel_equal_the_rows_the_turn_committed() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let thought = ids_of("They want seven lines and nothing else.");
    let answer = ids_of("Monday\nTuesday");
    let canned = Canned::serve(a_thinking_turn(&thought, &answer), 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "agree");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("the days, please")], &mut sink)
        .unwrap();
    let ok = engine.run_turn(&mut session, &mut sink).unwrap();

    let mut streamed_reasoning = String::new();
    let mut streamed_text = String::new();
    let mut streamed_call = String::new();
    for (target, text) in deltas(&sink) {
        match target {
            DeltaTarget::Reasoning => streamed_reasoning.push_str(text),
            DeltaTarget::Text => streamed_text.push_str(text),
            DeltaTarget::ToolCall => streamed_call.push_str(text),
        }
    }
    assert!(
        streamed_call.is_empty(),
        "this turn writes no tool call: {streamed_call:?}"
    );
    let committed_reasoning: String = ok
        .items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let committed_text: String = ok
        .items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    assert!(!committed_reasoning.is_empty() && !committed_text.is_empty());
    assert_eq!(streamed_reasoning, committed_reasoning);
    assert_eq!(streamed_text, committed_text);
}

/// T13.5, closed: the body of a `<tool_call>` block reaches a head on a channel
/// of its own and never as assistant text.
///
/// The defect this replaces was visible and documented and lived in the protocol,
/// not in the head: `DeltaTarget` had two variants, so the engine announced
/// `<function=read>…` as `Text`, and every head then had a choice between showing
/// raw markup and guessing. Guessing is not available — a user message quoting
/// `<function=` at the model is the same string — so the boundary had to be kept
/// where it still exists, which is here, walking the ids.
#[test]
fn the_body_of_a_tool_call_is_never_announced_as_assistant_text() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let mut frames = vec![Frame::Token {
        id: THINK_OPEN,
        text: "",
    }];
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    let said = ids_of("Reading it now.");
    frames.extend(spoken_frames(&said));
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        // The server sends the literal with the token, which is the case that
        // matters: the boundary's own text must not reach a head either.
        text: "<tool_call>",
    });
    let body = ids_of("\n{\"name\": \"read\", \"arguments\": {\"path\": \"/tmp/a\"}}\n");
    frames.extend(spoken_frames(&body));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (4 + said.len() + body.len()) as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "agree");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("read /tmp/a")], &mut sink)
        .unwrap();
    engine.run_turn(&mut session, &mut sink).unwrap();

    let mut text = String::new();
    let mut call = String::new();
    for (target, t) in deltas(&sink) {
        match target {
            DeltaTarget::Text => text.push_str(t),
            DeltaTarget::ToolCall => call.push_str(t),
            DeltaTarget::Reasoning => {}
        }
    }
    assert_eq!(
        text.trim(),
        "Reading it now.",
        "the markup leaked onto the text channel"
    );
    assert!(
        !text.contains("\"name\""),
        "a head reading Text would have rendered the call body: {text:?}"
    );
    assert!(
        call.contains("\"path\": \"/tmp/a\""),
        "the call body reached no channel at all: {call:?}"
    );
    assert!(
        !call.contains("<tool_call>") && !call.contains("</tool_call>"),
        "the boundary's own literal was streamed: {call:?}"
    );
}

// --------------------------------------------------------------------------
// T23 — a frame that does not account for its own advance
// --------------------------------------------------------------------------

/// Build the script for a turn the server drops one token out of the middle of.
fn suppressed_script(head: &[u32], tail: &[u32]) -> Vec<Frame> {
    let mut frames = vec![
        Frame::Token {
            id: THINK_OPEN,
            text: "",
        },
        Frame::Token {
            id: THINK_CLOSE,
            text: "",
        },
    ];
    frames.extend(spoken_frames(head));
    // The token the server counted and sent no frame for.
    frames.push(Frame::Suppressed);
    frames.extend(spoken_frames(tail));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (2 + head.len() + 1 + tail.len()) as u64,
        n_prompt: 10,
        cache_n: 0,
    });
    frames
}

/// The server counts a token it never sends a frame for, and the operator keeps
/// the answer anyway.
///
/// `Frame::Suppressed` is llama.cpp's `process_token` skipping
/// `send_partial_response` on an incomplete UTF-8 tail after `slot.stats.n_gen`
/// has already advanced. The next frame then reads `advance 2, ids 1`, which is
/// the exact shape seen twice in the operator's log.
///
/// Two things are asserted together because either alone is the wrong fix:
/// **nothing after the bad frame is committed** — the guard did not move — and
/// **everything before it is**, so the turn is a seam rather than a loss.
#[test]
fn a_suppressed_token_costs_the_tail_of_a_turn_and_not_the_whole_of_it() {
    let _g = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let head = ids_of("Reading it now.");
    let tail = ids_of(" And then some more.");
    let canned = Canned::serve(suppressed_script(&head, &tail), 1);

    let dir = std::env::temp_dir().join(format!("letibot-t23-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    engine.frame_capture = letibot_turn::FrameCapture::to_dir(&dir);
    let mut session = session(&engine, "t23");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("go")], &mut sink)
        .unwrap();
    let before = session.ledger.tokens().len();

    let ok = engine
        .run_turn(&mut session, &mut sink)
        .expect("the accounted prefix is a usable turn");

    // The head was committed…
    assert!(
        session.ledger.tokens().len() > before,
        "the whole turn was thrown away again"
    );
    assert_eq!(
        ok.metrics.predicted_tokens as usize,
        2 + head.len(),
        "exactly the ids the counter accounted for, and not one more"
    );
    // …and the seam is visible rather than silent.
    assert!(
        ok.truncated,
        "a turn missing its tail is not a complete one"
    );
    let (reason, kept) = sink
        .events
        .iter()
        .find_map(|e| match e {
            TurnEvent::TurnInterrupted {
                reason,
                partial_kept,
                ..
            } => Some((reason.clone(), *partial_kept)),
            _ => None,
        })
        .expect("the interruption must be announced");
    assert!(reason.starts_with("frame_mismatch"), "{reason}");
    assert!(kept, "the partial was kept and must say so");
    // **And the sentence says WHICH fault it was** (the R12-shaped half): the server
    // withheld ids it counted, which is the UTF-8 gate, so the operator is told to check
    // the server's build and given the commit to check it against. Before this the line
    // was three bare numbers — *tokens_predicted N -> M carried K id(s)* — which names a
    // disagreement and no remedy.
    assert!(
        reason.contains("withheld"),
        "the reading is what the operator acts on: {reason}"
    );
    assert!(
        reason.contains("d10f94713"),
        "the one check this box can name, because the fix is a patch on a branch: {reason}"
    );
    assert!(
        reason.contains("glm-all"),
        "and the branch, so the check is one command: {reason}"
    );
    assert!(
        !reason.contains("over_sent"),
        "the wrong reading, and the opposite remedy: {reason}"
    );

    // The evidence T23 asked for, on disk, verbatim.
    let path = sink
        .warnings()
        .iter()
        .find(|(c, _)| *c == "frame_capture_written")
        .map(|(_, d)| d.rsplit(' ').next().unwrap().to_string())
        .expect("the capture path must be announced");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let offender: serde_json::Value = serde_json::from_str(v["frame"].as_str().unwrap()).unwrap();
    assert_eq!(
        offender["tokens_predicted"].as_u64().unwrap() as usize,
        2 + head.len() + 2,
        "the captured frame is the one that was refused"
    );
    assert_eq!(
        offender["tokens"].as_array().unwrap().len(),
        1,
        "…and it carried one id, which is the whole complaint"
    );
    assert!(
        !v["before"].as_array().unwrap().is_empty(),
        "the frames either side are the point of capturing at all"
    );
    assert!(
        !v["after"].as_array().unwrap().is_empty(),
        "the trailing frames are what say whether the id arrives late"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The guard is not relaxed, and this is the line that says so.
///
/// The id the refused frame carried, and every id after it, are absent from the
/// ledger. A change that starts accepting them makes the ledger stop being a
/// record of what the model produced — the one outcome the hash chain exists to
/// prevent — and it would pass every other test in this file.
#[test]
fn the_ids_after_an_unaccountable_frame_never_reach_the_ledger() {
    let _g = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let head = ids_of("Reading it now.");
    let tail = ids_of(" And then some more.");
    let canned = Canned::serve(suppressed_script(&head, &tail), 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    engine.frame_capture = letibot_turn::FrameCapture::disabled();
    let mut session = session(&engine, "t23-guard");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("go")], &mut sink)
        .unwrap();
    engine.run_turn(&mut session, &mut sink).unwrap();

    let committed = session.ledger.tokens();
    assert!(
        committed.ends_with(&head[head.len() - 1..]),
        "the ledger must end exactly where the accounting stopped"
    );
    assert!(
        !committed.windows(tail.len()).any(|w| w == tail.as_slice()),
        "ids from beyond the refused frame reached the ledger"
    );
    // And a capture that is switched off says so rather than passing silently.
    assert!(
        sink.warnings()
            .iter()
            .any(|(c, _)| *c == "frame_capture_disabled")
    );
}

/// Urgent, but not until `after` polls — so the interrupt lands mid-thought
/// rather than before the model has said anything.
struct After {
    left: usize,
    msgs: Vec<SteeringMessage>,
}

impl SteeringSource for After {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        if self.left > 0 {
            self.left -= 1;
            return None;
        }
        if self.msgs.is_empty() {
            None
        } else {
            Some(self.msgs.remove(0))
        }
    }
    fn give_back(&mut self, msgs: Vec<SteeringMessage>) {
        // A handed-back message goes to the front, so the next `try_next` finds it
        // first: it was taken before anything still in the vector.
        self.msgs.splice(0..0, msgs);
    }
}

/// **The whole chain, on the path the operator is actually on.**
///
/// The first attempt at this fix elided a stopped thought in the RENDERER, and
/// the renderer is not on that path: the next turn's prompt is
/// `session.ledger.tokens()` and items are never re-rendered on the ordinary
/// path. The row was marked correctly and the mark was never reached — which is
/// exactly what the operator saw when they asked "why it reprefills and includes
/// aborted reasoning then?".
///
/// So this asserts the LEDGER, which is the thing that gets replayed: a thought
/// stopped mid-flow costs the next turn a sentence, not the thought.
#[test]
fn a_stopped_thought_costs_the_next_turn_a_sentence_not_the_thought() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);

    // Open a reasoning block and fill it with the shape that caused this: a model
    // counting by hand, at length, never closing the block because it is stopped.
    let mut frames = vec![Frame::Token {
        id: THINK_OPEN,
        text: "",
    }];
    frames.extend(token_frames(&ids_of(
        "let me count them by hand + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0 + 0",
    )));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: 99,
        n_prompt: 10,
        cache_n: 0,
    });
    let canned = Canned::serve(frames, 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "abandoned");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("how many parens are there")], &mut sink)
        .unwrap();
    let before = session.ledger.len();

    // Let it think for a while, then stop it.
    let mut steering = After {
        left: 12,
        msgs: vec![SteeringMessage::urgent("ABORT")],
    };
    let _ = engine.run_turn_steered(&mut session, &mut sink, &mut steering);

    let reasoning: Vec<&TranscriptItem> = session
        .items
        .iter()
        .filter(|i| matches!(i, TranscriptItem::Reasoning { .. }))
        .collect();
    assert!(
        !reasoning.is_empty(),
        "the interrupted thought was kept as an item: {:?}",
        session.items
    );
    for item in &reasoning {
        let TranscriptItem::Reasoning {
            truncated, text, ..
        } = item
        else {
            unreachable!()
        };
        assert!(*truncated, "and marked: {text:?}");
        // **The record keeps the whole thing.** This is a projection, not a
        // deletion: the store, a resume and any later reader still see it.
        assert!(
            text.contains("+ 0"),
            "the item still carries what the model actually produced: {text:?}"
        );
    }

    // And the ledger — the thing the next turn is prefilled from — does not.
    let grew = session.ledger.len() - before;
    assert!(
        grew < 60,
        "a stopped thought must cost the next turn a sentence, not the thought: \
         the ledger grew by {grew} tokens"
    );
}

/// The server's own counter rides the stream: one event per frame that
/// advanced it, in order, and the value is the server's `tokens_predicted` —
/// the number a head shows to tell a hang from a model that is still
/// emitting.
#[test]
fn a_streaming_turn_reports_the_servers_counter_on_every_frame_that_advances_it() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let thought = ids_of("The user wants the days of the week.");
    let answer = ids_of("Monday");
    let canned = Canned::serve(a_thinking_turn(&thought, &answer), 1);

    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "counter");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("the days, please")], &mut sink)
        .unwrap();
    engine.run_turn(&mut session, &mut sink).unwrap();

    let counts: Vec<u64> = sink
        .events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::TokensGenerated { tokens, .. } => Some(*tokens),
            _ => None,
        })
        .collect();
    // One per generated token, in order, from the first to the turn's total.
    // The progress frame advanced nothing and emitted nothing, and the final
    // frame carries no ids, so the last event is the total, not past it.
    let total = (thought.len() + answer.len() + 2) as u64;
    assert_eq!(counts, (1..=total).collect::<Vec<_>>());
}

// --------------------------------------------------------------------------
// What a FAILED round is called
// --------------------------------------------------------------------------

/// A provider that was never reached, which is what a resolver failure produces.
///
/// Not an `OpenAiProvider`: this test is about the CLASS the engine gives the
/// failure, and a real provider would need a socket to fail against. What matters is
/// that the error is `BackendError::Unreachable`, which is what
/// `OpenAiProvider::complete` builds when `.send()` fails.
struct NeverReached(&'static str);

impl letibot_backend::MessagesBackend for NeverReached {
    fn caps(&self) -> BackendCaps {
        BackendCaps::METERED_API
    }
    fn name(&self) -> &str {
        "deepseek"
    }
    fn model(&self) -> &str {
        "deepseek-flash"
    }
    fn authority(&self) -> String {
        "api.deepseek.com".into()
    }
    fn complete(
        &self,
        _req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        Err(letibot_backend::BackendError::Unreachable(self.0.into()))
    }
}

/// **A connection that never happened is an IO failure, not a malformed answer.**
///
/// `Malformed` means *2xx, but not the shape expected* — so this class told a reader a
/// server had answered with garbage when no server had spoken. MEASURED 2026-10-01: the
/// operator watched
///
///   malformed http response: provider unreachable: io: failed to lookup address
///   information: Temporary failure in name resolution
///
/// with `127.0.0.1:8080` answering `/health` 200 and `letibot --status` reporting it
/// `ok`. Nothing failed, so nothing caught it: the retry policy treats both classes the
/// same, which is exactly why a wrong class survives — it costs a diagnosis, not a test.
///
/// The second half is the one that must not be lost: the message still says the provider
/// was unreachable, and still carries the resolver's own words.
#[test]
fn an_unreached_provider_is_an_io_failure_and_not_a_malformed_answer() {
    let _lock = serial();
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        eprintln!("SKIPPED: no vocabulary GGUF, so the engine cannot open a session");
        return;
    };
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    let idle = Canned::serve(Vec::new(), 0);
    let mut engine = engine(&renderer, &parser, idle.endpoint.clone());
    let mut session = session(&engine, "unreached-provider");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("hello")], &mut sink)
        .unwrap();

    let provider = NeverReached(
        "io: failed to lookup address information: Temporary failure in name resolution",
    );
    let mut steering = Once(vec![]);
    let err = engine
        .run_turn_messages(
            &mut session,
            &mut sink,
            &mut steering,
            &provider,
            "be terse",
            &[],
            None,
        )
        .expect_err("a provider that was never reached cannot have answered");

    match err {
        TurnFailure::Http(letibot_turn::HttpError::Io(e)) => {
            let msg = e.to_string();
            assert!(
                msg.contains("provider unreachable"),
                "the provider's own words are gone: {msg}"
            );
            assert!(
                msg.contains("Temporary failure in name resolution"),
                "the resolver's own words are gone: {msg}"
            );
            // And the class, read the way a person reads it.
            let shown = letibot_turn::HttpError::Io(e).to_string();
            assert!(
                !shown.contains("malformed"),
                "an unreachable host is still being called malformed: {shown}"
            );
        }
        TurnFailure::Http(letibot_turn::HttpError::Malformed(m)) => {
            panic!("a connection that never happened was reported as a malformed answer: {m}")
        }
        other => panic!("a provider failure must arrive as TurnFailure::Http, got {other:?}"),
    }
}
