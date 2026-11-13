//! The compaction turn, against the canned server. Re-sending the previous prompt plus the
//! instruction is a property of the prompt, so what is asserted here is that the operation
//! appends it, runs one normal turn, reads the summary, and surfaces reuse numbers and tool calls.

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

use support::canned::{Canned, Frame, Reply};
use support::chatml::{ChatMlParser, ChatMlRenderer};

/// The summary turn with the ladder switched off, which is the shape every test here wants: a
/// canned server answers on the first call, and a fixture that waited would be measuring the
/// policy rather than the compaction.
fn no_retries() -> letibot_http::retry::Ladder {
    letibot_http::retry::Ladder::new(letibot_http::retry::Policy::none())
}

// Qwen's own ids, so the fixture and the vocabulary agree (same values the
// engine-decision tests pin).
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

fn vocab() -> std::sync::Arc<Vocab> {
    static VOCAB: OnceLock<std::sync::Arc<Vocab>> = OnceLock::new();
    VOCAB
        .get_or_init(|| {
            // One home for this path: `letibot_tokencore::apparatus`. It was
            // written out in seven crates, and `LETIBOT_VOCAB_GGUF` now wins
            // unconditionally there rather than being a hint.
            let p = letibot_tokencore::apparatus::gguf_path();
            assert!(p.is_file(), "no vocabulary GGUF at {}", p.display());
            std::sync::Arc::new(letibot_llama::load(&p).expect("the vocabulary must load"))
        })
        .clone()
}

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn engine(endpoint: Endpoint) -> TurnEngine {
    TurnEngine::new(
        vocab(),
        std::sync::Arc::new(ChatMlRenderer::default()),
        std::sync::Arc::new(ChatMlParser),
        endpoint,
        BackendCaps::OWN_SERVER,
        "canned",
        serde_json::json!({}),
    )
    .expect("the fixture dialect resolves against Qwen's vocabulary")
}

fn session(engine: &TurnEngine, tag: &str) -> Session {
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

/// Two ordinary turns, then the compaction turn: the instruction went into the ledger as a
/// `System` update after the history, the summary is the turn's visible text verbatim, and the
/// reuse numbers come off the turn's own metrics with no reconciliation between them.
#[test]
fn compaction_appends_the_instruction_and_reads_the_summary_off_the_turn() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

    // Turn one: an ordinary answer over a session with one user item.
    let answer = ids_of("the answer was forty-two");
    let first = a_summary_turn(&ids_of("count the things"), &answer, 20, 18);
    let canned = Canned::serve(first, 1);
    let mut engine1 = engine(canned.endpoint.clone());
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
    let mut engine2 = engine(canned.endpoint.clone());
    let outcome = letibot_turn::run_compaction(
        &mut engine2,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
        &mut no_retries(),
        None,
    )
    .unwrap();

    assert_eq!(outcome.summary, summary_text, "{:?}", outcome.summary);
    assert_eq!(outcome.tool_calls, 0);
    // What the invariant permitted: the first turn's prompt plus its committed
    // generation - proven identical over that span, whatever the server did.
    assert_eq!(outcome.reusable, witness.covered_len as u64);
    assert_eq!(outcome.cached_tokens, 39);
    assert!(outcome.generated_tokens > 0);

    // The ledger carries the instruction as a system update - distinguishable
    // from anything anybody typed - and the turn's own items after it: the
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
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

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
    let mut engine = engine(canned.endpoint.clone());
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
        &mut no_retries(),
        None,
    )
    .unwrap();
    assert_eq!(outcome.tool_calls, 1, "{outcome:?}");
    assert!(outcome.summary.is_empty(), "{:?}", outcome.summary);
}

/// A refused summary is taken back the same way a failed fold is, and the WITNESS with it: it names
/// a prompt and a generation the ledger no longer holds, so a restored conversation that kept it
/// reports the rollback as a divergence on its next turn.
#[test]
fn a_refused_summary_is_taken_back_with_the_witness_it_left() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

    let mut call = vec![Frame::Progress {
        total: 30,
        processed: 30,
    }];
    call.extend(spoken_frames(&[THINK_CLOSE]));
    call.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "",
    });
    let body = ids_of("\n{\"name\": \"read\", \"arguments\": {\"path\": \"/tmp/a\"}}\n");
    call.extend(body.iter().map(|id| Frame::Token { id: *id, text: "" }));
    call.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "",
    });
    call.extend(spoken_frames(&[IM_END]));
    call.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (body.len() + 4) as u64,
        n_prompt: 30,
        cache_n: 0,
    });

    // One answer first, so there is a witness for the refusal to destroy and a conversation for it
    // to leave in place.
    let canned = Canned::serve_each(
        vec![a_summary_turn(&[], &ids_of("the answer"), 20, 0), call],
        2,
    );
    let mut engine = engine(canned.endpoint.clone());
    let mut session = session(&engine, "compact-refused");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("do a thing")], &mut sink)
        .unwrap();
    engine
        .run_turn(&mut session, &mut sink)
        .expect("the first turn answers");

    let mark = session.items.len();
    let tokens = session.ledger.tokens().to_vec();
    let witness = session.witness().cloned();
    assert!(
        witness.is_some(),
        "a turn that ran leaves the prefix witness the next one is checked against"
    );

    let outcome = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
        &mut no_retries(),
        None,
    )
    .expect("the summary turn itself answers");
    assert_eq!(outcome.tool_calls, 1, "{outcome:?}");
    assert!(
        session.items.len() > mark,
        "the turn it refuses is in the transcript until the caller takes it back"
    );

    letibot_turn::undo_refused_summary(&mut session, mark, witness.clone());
    assert_eq!(session.items.len(), mark, "{:#?}", session.items);
    assert_eq!(session.ledger.rows().len(), mark, "one row per item");
    assert_eq!(
        session.ledger.tokens(),
        tokens.as_slice(),
        "the region is where the refusal found it"
    );
    assert_eq!(
        session.witness().cloned(),
        witness,
        "the witness still describes the prompt the ledger holds"
    );
}

/// A summary turn that keeps saying nothing fails the compaction once the salvage budget is
/// spent, and nothing is reduced: the whole failed fold is rolled back, so the next attempt starts
/// where this one did. The fourth failure is `SalvageExhausted` and propagates.
#[test]
fn a_summary_turn_that_never_says_anything_exhausts_the_salvage_and_fails_the_compaction() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

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
    let mut engine = engine(canned.endpoint.clone());
    let mut session = session(&engine, "compact-fail");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("summarise")], &mut sink)
        .unwrap();
    let before_compaction = session.ledger.tokens().to_vec();
    let items_before = session.items.len();

    let err = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
        &mut no_retries(),
        None,
    )
    .unwrap_err();
    match err {
        TurnFailure::SalvageExhausted { streak, .. } => {
            assert_eq!(streak, 4, "the cap is three salvages plus the spent turn");
        }
        other => panic!("the spent budget must propagate as itself: {other:?}"),
    }

    // The failed fold left nothing behind - no assistant row, no instruction, and none of the three
    // salvage notices either: what it appended is undone, or a session that keeps failing carries
    // one more copy of the instruction per attempt.
    assert_eq!(session.items.len(), items_before, "{:#?}", session.items);
    assert_eq!(session.ledger.rows().len(), items_before);
    assert_eq!(
        session.ledger.tokens(),
        before_compaction.as_slice(),
        "the region is where the failure found it"
    );
    session
        .ledger
        .verify_chain()
        .expect("the chain still holds");
}

/// A fold that fails leaves the transcript as it was, however often it fails. The instruction is
/// appended before the turn runs and must not stay, or failed folds leave one copy each.
/// Counted, never measured.
#[test]
fn a_failed_fold_leaves_no_instruction_behind() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

    const ATTEMPTS: usize = 60;
    // Every attempt is refused, so each one is a single failing request: the loop is about what the
    // transcript keeps, not about the salvage budget.
    let canned = Canned::serve_replies(
        vec![Reply::Status {
            code: 500,
            body: r#"{"error":{"message":"no"}}"#.into(),
        }],
        ATTEMPTS,
    );
    let mut engine = engine(canned.endpoint.clone());
    let mut session = session(&engine, "compact-stacked");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("summarise")], &mut sink)
        .unwrap();
    let items = session.items.len();
    let rows = session.ledger.rows().len();
    let head = session.ledger.head();

    for attempt in 0..ATTEMPTS {
        let err = letibot_turn::run_compaction(
            &mut engine,
            &mut session,
            &mut sink,
            &letibot_turn::compaction::Answerer::Local,
            &mut no_retries(),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, TurnFailure::Http(_)),
            "attempt {attempt} failed as {err:?}"
        );
        assert_eq!(
            session.items.len(),
            items,
            "attempt {attempt} left {} item(s) behind, so the instruction stacks",
            session.items.len() - items
        );
        assert_eq!(session.ledger.rows().len(), rows, "attempt {attempt}");
        assert_eq!(session.ledger.head(), head, "attempt {attempt}");
    }
    session
        .ledger
        .verify_chain()
        .expect("the chain still holds");
}

/// The frames of the live finding, replayed: the stream stops with a normal `eos` while the
/// reasoning block is still open and nothing but reasoning was produced - `UnfinishedReasoning`.
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

/// The compaction path gets the tool loop's salvage: a summary turn that stops inside its
/// reasoning block is answered with the notice and taken again, and the compaction completes.
/// Without the loop the session stayed at the wall with `/compact` retrying into the same wall.
#[test]
fn an_unfinished_reasoning_summary_turn_is_salvaged_and_the_compaction_completes() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

    let summary_text = "decided: answer is forty-two; no files changed; open: none";
    let salvaged = a_summary_turn(
        &ids_of("gather the record, again"),
        &ids_of(summary_text),
        40,
        39, // the server reused everything but the notice suffix
    );
    // Request one ends unfinished; request two is the retry and answers.
    let canned = Canned::serve_each(vec![an_unfinished_reasoning_turn(30), salvaged], 2);
    let mut engine = engine(canned.endpoint.clone());
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
        &mut no_retries(),
        None,
    )
    .expect("the salvage takes the turn again, and the retry answers");

    assert_eq!(outcome.summary, summary_text, "{:?}", outcome.summary);
    assert_eq!(outcome.tool_calls, 0);

    // What the failed turn left behind: nothing. No assistant row, no reasoning row, no
    // half-thought - a failed turn commits no items, so the transcript between the instruction
    // and the notice is exactly where the failed turn would have spoken and is not.
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
    // it says to write the summary, not to reason "after" - a model whose thinking
    // always comes first cannot.
    assert!(text.contains("Write the summary now"), "{text}");

    // And the retry's answer is the last item, one turn's worth of rows after
    // the notice - the region grew by the instruction, the notice and the
    // successful turn, and by nothing from the failed one.
    assert_eq!(
        outcome.summary,
        match session.items.last().unwrap() {
            TranscriptItem::Assistant { text, .. } => text.as_str(),
            other => panic!("{other:?}"),
        }
    );
    assert!(session.ledger.tokens().len() > before_compaction.len());

    // The reuse the auto_compact message promises, checked rather than assumed: the failed turn
    // committed nothing, so the retry's prompt extended the previous prompt-plus-generation
    // exactly, and the server was asked to re-prefill only the notice.
    assert!(
        !sink
            .warnings()
            .iter()
            .any(|(c, _)| *c == "prefix_divergence"),
        "the retry's prompt broke the prefix invariant: {:?}",
        sink.warnings()
    );
}

/// A metered session's summary goes to the provider, not to the daemon's own model: an
/// unconditional `run_turn` sent a deepseek conversation's summaries to the local qwen with the
/// whole history in front of them, which the provider's own 400 named.
#[test]
fn a_summary_turn_goes_to_the_provider_when_there_is_one() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    use letibot_backend::{
        BackendCaps, BackendError, Completion, Delta, MessagesBackend, StreamFlow, TurnRequest,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
        text: String,
        /// How many tools the request carried, recorded off the real `TurnRequest`: the summary
        /// turn must send the session's OWN prefix, and the tool list is the only angle from
        /// which that is visible.
        tools_seen: AtomicUsize,
        /// The tools themselves, not just the count - a summary sent under a
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
                thoughts: Vec::new(),
            })
        }
    }

    let _lock = serial();
    // A local server that must NOT be asked: it serves nothing, so any request
    // to it fails the turn and the test with it.
    let idle = Canned::serve(Vec::new(), 0);
    let mut engine = engine(idle.endpoint.clone());
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
    let outcome = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &answerer,
        &mut no_retries(),
        None,
    )
    .unwrap();

    assert_eq!(
        backend.calls.load(Ordering::Relaxed),
        1,
        "the summary did not reach the provider"
    );
    // The compaction request carries the session's tools: a summary cannot call one, but the
    // tool list is what makes its prefix byte-identical to every other turn's, so a local server
    // reuses the cached prefix instead of re-reading the largest conversation cold.
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

/// A summary turn that meets a `503` is taken again - the compaction is a provider call like
/// any other. The retry is safe because the instruction was appended once, before the salvage
/// loop, which is why the ladder lives inside `run_compaction` rather than around it.
#[test]
fn a_summary_turn_is_taken_again_when_the_endpoint_says_try_again() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

    let summary_text = "decided: the fold landed on the second try; no files changed";
    let frames = a_summary_turn(&ids_of("gather the record"), &ids_of(summary_text), 40, 39);
    // The endpoint answers `503 Loading model` once - the status a server reloading a
    // sharded GGUF answers - and the summary on the second call.
    let canned = Canned::serve_replies(
        vec![
            Reply::Status {
                code: 503,
                body: "Loading model".into(),
            },
            Reply::Frames(frames),
        ],
        2,
    );
    let mut engine = engine(canned.endpoint.clone());
    let mut session = session(&engine, "compact-retry");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &[user("count the things")], &mut sink)
        .unwrap();

    let mut ladder = letibot_http::retry::Ladder::new(letibot_http::retry::Policy::new(1));
    let outcome = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
        &mut ladder,
        None,
    )
    .expect("the second call answered");
    assert_eq!(outcome.summary, summary_text, "{:?}", outcome.summary);
    assert_eq!(ladder.attempts(), 1, "one call failed, and one was retried");
    // The instruction is in the transcript ONCE - the retry re-ran the turn, not the whole
    // function.
    let instructions = session
        .items
        .iter()
        .filter(|i| {
            matches!(i, TranscriptItem::System { text, .. } if text == letibot_turn::SUMMARY_INSTRUCTION)
        })
        .count();
    assert_eq!(
        instructions, 1,
        "a retried summary turn appended its instruction twice"
    );
}

/// A conversation long enough that the facts in it are a count rather than a handful: eighty
/// operator turns and forty files read. Appended rather than run - what a record is measured
/// against is the CALLS, and no turn has to have happened for them to be in the transcript.
fn a_long_fake_conversation() -> Vec<TranscriptItem> {
    let mut items: Vec<TranscriptItem> = Vec::new();
    for i in 0..80 {
        items.push(user(&format!("step {i}: carry on")));
        items.push(TranscriptItem::Assistant {
            text: "working on it".into(),
            tool_calls: vec![],
            truncated: false,
        });
    }
    for i in 0..40 {
        items.push(TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![letibot_transcript::ToolCall {
                id: format!("r{i}"),
                name: "read".into(),
                arguments: format!(r#"{{"path":"crates/turn/src/file_{i}.rs"}}"#),
            }],
            truncated: false,
        });
        items.push(TranscriptItem::ToolResult {
            call_id: format!("r{i}"),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "the file's body, which does not survive the record".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        });
    }
    items
}

/// The sized-record loop, end to end. The conversation holds forty files; the first record names one of
/// them and says nothing else; the daemon measures it, tells the model exactly which paths it
/// dropped, and takes the record it gets back. Two requests and no third: the ask happens once.
#[test]
fn a_record_that_drops_most_of_the_conversation_is_asked_for_again_with_the_gap_named() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

    let thin = "## Objective\n- carry the work across\n## Relevant Files\n\
                 - crates/turn/src/file_0.rs: read\n";
    let mut whole = String::from("## Objective\n- carry the work across\n## Relevant Files\n");
    for i in 0..40 {
        whole.push_str(&format!(
            "- crates/turn/src/file_{i}.rs: read for the budget\n"
        ));
    }

    // Two requests, and the server stops: a third answer would have to come from nowhere.
    let canned = Canned::serve_each(
        vec![
            a_summary_turn(&ids_of("gather the record"), &ids_of(thin), 400, 380),
            a_summary_turn(
                &ids_of("gather the record again"),
                &ids_of(&whole),
                420,
                400,
            ),
        ],
        2,
    );
    let mut engine = engine(canned.endpoint.clone());
    let mut session = session(&engine, "compact-fidelity");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &a_long_fake_conversation(), &mut sink)
        .unwrap();

    let budget = letibot_turn::summary_budget(262_144, 6_880, 16_384, 200_000);
    let ask = letibot_turn::SummaryAsk {
        budget,
        carried: "",
        template: None,
    };
    let outcome = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
        &mut no_retries(),
        Some(&ask),
    )
    .expect("the second answer is the record");

    // The record that stands in for the conversation is the second answer.
    assert_eq!(outcome.summary, whole, "{:?}", outcome.summary);
    let fidelity = outcome.fidelity.expect("the record was measured");
    assert!(fidelity.passed(), "{}", fidelity.line());
    assert_eq!(fidelity.level("files").map(|l| l.total), Some(40));
    assert_eq!(fidelity.level("files").map(|l| l.kept), Some(40));
    assert_eq!(
        outcome.budget,
        Some(budget),
        "the budget is disclosed, not implied"
    );

    // The ask that got it there: a user row, not the operator's words, naming what went missing.
    let gap = session
        .items
        .iter()
        .find_map(|i| match i {
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts,
            } => match &parts[..] {
                [UserPart::Text { text }] if text.contains("file_1.rs") => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("the gap was said to the model");
    assert!(gap.contains("files 1 of 40"), "{gap}");
    assert!(gap.contains("file_2.rs"), "{gap}");

    // And the instruction the session was given is the SIZED one: the number, and the demand for a
    // line per thing rather than the word that produced 2,250 tokens for 3,590 rows.
    let instruction = session
        .items
        .iter()
        .find_map(|i| match i {
            TranscriptItem::System {
                text,
                origin: SystemOrigin::Update,
            } if text.contains("One line per thing") => Some(text.clone()),
            _ => None,
        })
        .expect("the sized instruction is in the ledger as a system update");
    assert!(
        instruction.contains(&budget.tokens.to_string()),
        "{instruction}"
    );
    assert!(!instruction.to_lowercase().contains("compact"));
}

/// The other half of the rule: a compaction asked for by nothing - the overrun halves, and every
/// test above - is not measured and not re-asked.
#[test]
fn a_record_nobody_asked_for_a_size_is_not_measured_and_not_re_asked() {
    let Some(_) = letibot_llama::apparatus::present_gguf() else {
        return;
    };
    let _lock = serial();

    let thin = "## Objective\n- carried across\n";
    let canned = Canned::serve(
        a_summary_turn(&ids_of("gather the record"), &ids_of(thin), 40, 39),
        1,
    );
    let mut engine = engine(canned.endpoint.clone());
    let mut session = session(&engine, "compact-unsized");
    let mut sink = RecordingSink::new();
    session
        .append_items(&engine, &a_long_fake_conversation(), &mut sink)
        .unwrap();

    let outcome = letibot_turn::run_compaction(
        &mut engine,
        &mut session,
        &mut sink,
        &letibot_turn::compaction::Answerer::Local,
        &mut no_retries(),
        None,
    )
    .expect("one request, one answer");
    assert_eq!(outcome.summary, thin);
    assert!(outcome.fidelity.is_none());
    assert!(outcome.budget.is_none());
}
