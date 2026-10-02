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
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let witness = session
        .witness()
        .expect("a completed turn left a witness")
        .clone();
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
    let outcome = letibot_turn::run_compaction(
        &mut engine2,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
    )
    .unwrap();

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
            matches!(
                i,
                TranscriptItem::System {
                    origin: SystemOrigin::Update,
                    ..
                }
            ) && match i {
                TranscriptItem::System { text, .. } => text == letibot_turn::SUMMARY_INSTRUCTION,
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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

    let outcome = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
    )
    .unwrap();
    assert_eq!(outcome.tool_calls, 1, "{outcome:?}");
    assert!(outcome.summary.is_empty(), "{:?}", outcome.summary);
}

/// A summary turn that keeps saying nothing fails the compaction once the
/// salvage budget is spent, and nothing is reduced: the instruction and the
/// salvage notices stay in the ledger, which is where the next attempt appends
/// over them.
///
/// The salvage is the tool loop's, bounded by the engine's own budget (cap 3):
/// three say-nothing turns are answered with a notice and retried, and the
/// fourth comes back `SalvageExhausted`, which is not a say-nothing failure and
/// so propagates. That propagation is what keeps `auto_compact_failed` honest —
/// a compaction that did not run must say so, not vanish into retries.
#[test]
fn a_summary_turn_that_never_says_anything_exhausts_the_salvage_and_fails_the_compaction() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);

    // All budget spent thinking; `finish_reason: length`, no content. The same
    // frames every time: a model that truncates once usually truncates again,
    // which is the reason the budget exists.
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
    // Four requests: three salvages, then the turn whose failure is the spent
    // budget itself. The script cycles, so one list covers all of them.
    let canned = Canned::serve_each(vec![frames], 4);
    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "compact-fail");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("summarise")], &mut sink)
        .unwrap();
    let before_compaction = session.ledger.tokens().to_vec();

    let err = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
    )
    .unwrap_err();
    match err {
        TurnFailure::SalvageExhausted { streak, .. } => {
            assert_eq!(streak, 4, "the cap is three salvages plus the spent turn");
        }
        other => panic!("the spent budget must propagate as itself: {other:?}"),
    }

    // The failed turns committed nothing — no assistant item, no reasoning item,
    // no dangling half-summary anywhere. What the region gained is exactly the
    // instruction and the three notices, which is also the prefix-reuse promise
    // kept: the retry's prompt is the previous prompt plus those rows, so the
    // server is re-prefilling only the notices, never the conversation.
    assert!(session.items.iter().all(|i| !matches!(
        i,
        TranscriptItem::Assistant { .. } | TranscriptItem::Reasoning { .. }
    )));
    let notices = session
        .items
        .iter()
        .filter(|i| match i {
            TranscriptItem::User { parts, .. } => match &parts[..] {
                [UserPart::Text { text }] => {
                    // The length notice, not the unfinished-reasoning one: both
                    // open the same sentence, and this is the phrase that is
                    // only in the former.
                    text.contains("spent its whole output on reasoning")
                }
                _ => false,
            },
            _ => false,
        })
        .count();
    assert_eq!(notices, 3, "one notice per salvaged turn");
    assert!(matches!(
        session.items.last().unwrap(),
        TranscriptItem::User { .. }
    ));
    assert!(
        session.ledger.tokens().len() > before_compaction.len(),
        "the instruction and the notices are in the region"
    );
}

/// The frames of the live finding, replayed: the stream stops with a normal
/// `eos` while the reasoning block is still open and nothing but reasoning was
/// produced — R7's `UnfinishedReasoning`, the shape the operator's session hit
/// on 2026-09-15 at the context wall.
fn an_unfinished_reasoning_turn(n_prompt: u64) -> Vec<Frame> {
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    let thought = ids_of("mid-summary, the think block never closed, so no");
    frames.extend(token_frames(&thought));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: 1 + thought.len() as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

fn token_frames(ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token { id: *id, text: "" })
        .collect()
}

/// The compaction path gets the tool loop's R7 salvage: a summary turn that
/// stops inside its own reasoning block is answered with the notice and taken
/// again, and the compaction completes. Before this loop existed the whole
/// compaction failed, `auto_compact_failed` fired, and the session stayed at
/// the wall with `/compact` retrying into the same wall.
#[test]
fn an_unfinished_reasoning_summary_turn_is_salvaged_and_the_compaction_completes() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);

    let summary_text = "decided: answer is forty-two; no files changed; open: none";
    let salvaged = a_summary_turn(
        &ids_of("gather the record, again"),
        &ids_of(summary_text),
        40,
        39, // the server reused everything but the notice suffix
    );
    // Request one ends unfinished; request two is the retry and answers.
    let canned = Canned::serve_each(vec![an_unfinished_reasoning_turn(30), salvaged], 2);
    let mut engine = engine(&renderer, &parser, canned.endpoint.clone());
    let mut session = session(&engine, "compact-salvage");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("summarise")], &mut sink)
        .unwrap();
    let before_compaction = session.ledger.tokens().to_vec();

    let outcome = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
    )
    .expect("the salvage takes the turn again, and the retry answers");

    assert_eq!(outcome.summary, summary_text, "{:?}", outcome.summary);
    assert_eq!(outcome.tool_calls, 0);

    // What the failed turn left behind: nothing. No assistant row, no reasoning
    // row, no dangling half-thought — §5.7 and R7 commit no items, so the
    // transcript between the instruction and the notice is exactly where the
    // failed turn would have spoken and is not.
    let instruction_at = session
        .items
        .iter()
        .position(|i| {
            matches!(
                i,
                TranscriptItem::System {
                    origin: SystemOrigin::Update,
                    ..
                }
            ) && match i {
                TranscriptItem::System { text, .. } => text == letibot_turn::SUMMARY_INSTRUCTION,
                _ => false,
            }
        })
        .expect("the instruction is in the ledger as a system update");
    assert!(
        session.items[..instruction_at].iter().all(|i| !matches!(
            i,
            TranscriptItem::Assistant { .. } | TranscriptItem::Reasoning { .. }
        )),
        "nothing the failed turn said is in the ledger"
    );

    // The notice is a user item directly after the instruction, carrying the
    // ask the way the tool loop's EmptyLength arm carries it.
    let letibot_transcript::TranscriptItem::User { parts, .. } = &session.items[instruction_at + 1]
    else {
        panic!(
            "the salvage notice is a user item, got {:?}",
            session.items[instruction_at + 1]
        );
    };
    let letibot_transcript::UserPart::Text { text } = &parts[0] else {
        panic!("the notice is text, got {:?}", parts[0]);
    };
    assert_eq!(text, letibot_turn::UNFINISHED_REASONING_NOTICE);
    assert!(text.contains("before any reasoning"), "{text}");

    // And the retry's answer is the last item, one turn's worth of rows after
    // the notice — the region grew by the instruction, the notice and the
    // successful turn, and by nothing from the failed one.
    assert_eq!(
        outcome.summary,
        match session.items.last().unwrap() {
            TranscriptItem::Assistant { text, .. } => text.as_str(),
            other => panic!("{other:?}"),
        }
    );
    assert!(session.ledger.tokens().len() > before_compaction.len());

    // The reuse the auto_compact message promises, checked rather than assumed:
    // the failed turn committed nothing, so the retry's prompt extended the
    // previous prompt-plus-generation exactly, and the exact-form prefix check
    // — which raises `prefix_divergence` when it does not — had nothing to say.
    // The server was asked to re-prefill only the notice, never the
    // conversation.
    assert!(
        !sink
            .warnings()
            .iter()
            .any(|(c, _)| *c == "prefix_divergence"),
        "the retry's prompt broke the prefix invariant: {:?}",
        sink.warnings()
    );
}

/// **A metered session's summary goes to the provider, not to the daemon's own
/// model.**
///
/// `run_compaction` called `TurnEngine::run_turn` unconditionally, and that is
/// the local endpoint — the engine holds one endpoint and knows nothing about a
/// provider. So a conversation on deepseek had its summaries sent to the local
/// qwen with the whole history in front of them. Measured on the operator's box,
/// 2026-09-20, two lines apart in one log: `maximum context length is 1048576`
/// from deepseek for the turn, then `exceeds the available context size
/// (262144)` from qwen for the SUMMARY. The second is this bug.
#[test]
fn a_summary_turn_goes_to_the_provider_when_there_is_one() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    use letibot_backend::{
        BackendCaps, BackendError, Completion, Delta, MessagesBackend, StreamFlow, TurnRequest,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
        text: String,
        /// **How many tools the request carried**, recorded off the real
        /// `TurnRequest` rather than inferred. The summary turn must send the
        /// session's OWN prefix, and this is the only angle from which that is
        /// visible: the tool list is what makes the prefix byte-identical to every
        /// other turn's, which is what lets a local server reuse its cached prefix
        /// instead of re-reading the whole conversation cold.
        tools_seen: AtomicUsize,
        /// The tools themselves, not just the count — a summary sent under a
        /// *different* tool list would still be a different prefix, and a count
        /// cannot tell that apart from the right one.
        names_seen: std::sync::Mutex<Vec<String>>,
    }
    impl MessagesBackend for Counting {
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
            req: &TurnRequest<'_>,
            on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
        ) -> Result<Completion, BackendError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.tools_seen
                .store(req.tools_json.len(), Ordering::Relaxed);
            *self.names_seen.lock().unwrap_or_else(|e| e.into_inner()) = req.tools_json.to_vec();
            on_delta(&Delta::Text(self.text.clone()));
            Ok(Completion {
                text: self.text.clone(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish: letibot_backend::Finish::Stop,
                cost: letibot_backend::TurnCost {
                    meter: letibot_backend::Meter::Metered,
                    prompt_tokens: 11,
                    cached_tokens: 0,
                    generated_tokens: 7,
                    wall_ms: 5,
                    micros_usd: Some(3),
                },
                raw_usage: None,
            })
        }
    }

    let _lock = serial();
    let (renderer, parser) = (ChatMlRenderer::default(), ChatMlParser);
    // A local server that must NOT be asked: it serves nothing, so any request
    // to it fails the turn and the test with it.
    let idle = Canned::serve(Vec::new(), 0);
    let mut engine = engine(&renderer, &parser, idle.endpoint.clone());
    let mut session = session(&engine, "compact-provider");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("count the things")], &mut sink)
        .unwrap();

    let summary = "decided: the provider answered; open: none";
    // The session's own tool list, as a prefix would carry it.
    let tools = vec![
        "{\"name\":\"bash\"}".to_string(),
        "{\"name\":\"read\"}".to_string(),
    ];
    let backend = Counting {
        calls: AtomicUsize::new(0),
        text: summary.to_string(),
        tools_seen: AtomicUsize::new(usize::MAX),
        names_seen: std::sync::Mutex::new(Vec::new()),
    };
    let answerer = letibot_turn::compaction::Answerer::Provider {
        backend: &backend,
        system: "you are a summariser",
        tools_json: &tools,
    };
    let outcome =
        letibot_turn::run_compaction(&mut engine, &mut session, &mut sink, &answerer).unwrap();

    assert_eq!(
        backend.calls.load(Ordering::Relaxed),
        1,
        "the summary did not reach the provider"
    );
    // **The compaction request carries the session's tools**, and that is a
    // deliberate choice rather than dead weight: a summary cannot call one, but the
    // tool list is what makes its prefix byte-identical to every other turn's, so a
    // local server reuses the cached prefix instead of re-reading the largest
    // conversation in the session cold.
    //
    // The opposite was tried and reverted 2026-10-02, on a rule that turned out not
    // to be the API's behaviour: the vendor's guide says `reasoning_content` must be
    // passed back when a request carries `tools`, and all five permutations measured
    // against the live API answered 200 — and a no-tools compaction was still refused
    // for not passing `reasoning_content` back. See `Answerer::Provider`'s docs.
    //
    // Asserted on the request the backend actually received, and on the tool list
    // itself rather than its length: a summary sent under a DIFFERENT list is still a
    // different prefix, which is the failure this exists to catch.
    assert_eq!(
        backend.tools_seen.load(Ordering::Relaxed),
        tools.len(),
        "a summary turn must send the session's own tools, or its prefix will not \
         match the one the server has cached"
    );
    assert_eq!(
        *backend.names_seen.lock().unwrap_or_else(|e| e.into_inner()),
        tools,
        "the summary sent a different tool list from the session's"
    );
    assert_eq!(outcome.summary, summary);
}
