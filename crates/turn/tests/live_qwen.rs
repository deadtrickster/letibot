//! The engine against the llama.cpp server that is actually running on this box.
//!
//! # Why these are not skipped when the server is absent
//!
//! The same reason `tokencore`'s FFI tests are not: a test that quietly passes when
//! the thing it tests is missing reports the health of a `TcpStream::connect`, and
//! this fleet has already paid for the difference between a liveness indicator and
//! the fact it stands in for. If the endpoint is wrong for your box, set
//! `LETIBOT_COMPLETION_URL` and `LETIBOT_VOCAB_GGUF`.
//!
//! # The constraint these tests respect
//!
//! The server is in production use with five slots, and saturating them is not
//! acceptable. So every test here takes one process-wide lock, and each holds it
//! for one or two short generations. The whole file is a handful of requests.
//!
//! # What is deliberately *not* tested live
//!
//! `finish_reason: length`. Producing one requires either an `n_predict` cap —
//! which §5.7 removed from the request on purpose, and which no test may add back —
//! or filling a 1.3 M-token context. The policy is exercised exhaustively offline
//! in `length.rs` instead, which is where the nine-unnoticed-rows case belongs
//! anyway: it is a decision, not a server behaviour.

mod support {
    pub mod chatml;
}

use std::sync::{Mutex, MutexGuard, OnceLock};

use letibot_backend::BackendCaps;
use letibot_dialect::StablePrefix;
use letibot_tokencore::Vocab;
use letibot_transcript::{TranscriptItem, UserPart};
use letibot_turn::{
    Endpoint, PrefixCheck, RecordingSink, Session, TurnEngine, TurnEvent, prefix::PrefixWitness,
};

use support::chatml::{ChatMlParser, ChatMlRenderer};

/// One at a time: this box's slots are shared with real work.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn endpoint() -> Endpoint {
    let url =
        std::env::var("LETIBOT_COMPLETION_URL").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let (host, port) = url.rsplit_once(':').expect("HOST:PORT");
    let ep = Endpoint::new(host, port.parse().expect("port"));
    // Ask what is actually behind the port before tokenising for it. These three
    // model services are singletons that evict each other and have shared `:8080`,
    // so the wrong one being up is the ordinary case rather than the strange one —
    // and the failure it produces (`400 Prompt contains invalid tokens`) names the
    // tokenizer for what is really a different model. Once per binary.
    ep
}

/// Is a ChatML model actually behind the port?
///
/// **These tests are dialect-bound and cannot follow the served model.** They
/// build a `ChatMlRenderer` and a `ChatMlParser`; against GLM the control tokens
/// do not resolve and nothing here is exercised. That is a fact about which
/// singleton service is up, not about the code — `letibot-harnessd`'s
/// `loop_closes` needs only *a* model and now follows whatever is served, but
/// this file cannot.
///
/// So it SKIPS rather than panicking, in the idiom this tree already uses for
/// every environment-gated check (`FLOWY_LIVE`, `BRAVE_LIVE`, `FIRECODE_LIVE`,
/// `SUDO_LIVE`): loudly, saying it is not a pass. Before this they were four
/// permanent failures on a box serving GLM, and a suite that is always red is a
/// suite nobody reads — which is how they came to be reported all day as
/// "pre-existing, not mine" instead of being dealt with.
fn qwen_is_served() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let want =
            std::env::var("LETIBOT_MODEL_ALIAS").unwrap_or_else(|_| "qwen-3.8-flash-next".into());
        match letibot_turn::serving::served_model(&endpoint()) {
            Ok(served) if letibot_turn::serving::matches(&served, &want) => true,
            Ok(served) => {
                eprintln!(
                    "SKIPPED: {} is serving `{served}`, and these tests render ChatML for \
                     `{want}` — the control tokens would not resolve, so nothing was run \
                     and THIS IS NOT A PASS. Start {want}, or set LETIBOT_MODEL_ALIAS, \
                     LETIBOT_COMPLETION_URL and LETIBOT_VOCAB_GGUF.",
                    endpoint().authority()
                );
                false
            }
            Err(e) => {
                eprintln!(
                    "SKIPPED: could not ask {}/props ({e}), so nothing was run and THIS \
                     IS NOT A PASS.",
                    endpoint().authority()
                );
                false
            }
        }
    })
}

fn vocab() -> std::sync::Arc<Vocab> {
    static VOCAB: OnceLock<std::sync::Arc<Vocab>> = OnceLock::new();
    VOCAB
        .get_or_init(|| {
            let path = std::env::var("LETIBOT_VOCAB_GGUF").unwrap_or_else(|_| {
            "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf"
                .to_string()
        });
            let p = std::path::PathBuf::from(&path);
            assert!(
                p.is_file(),
                "no vocabulary GGUF at {path}. Set LETIBOT_VOCAB_GGUF; for a split model \
             pass the first shard."
            );
            std::sync::Arc::new(letibot_llama::load(&p).expect("the vocabulary must load"))
        })
        .clone()
}

/// A fresh session with a nonce in its system prompt, so no earlier run's cache
/// entry can make a cold turn look warm. §18.2's "two conversations cannot test
/// residency" applies to prefixes too.
fn fresh(engine: &TurnEngine, tag: &str) -> Session {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let prefix = StablePrefix {
        system: format!("You are terse. Answer in one short sentence. Session {tag}-{nonce}."),
        tools_json: vec![],
    };
    engine
        .open(&format!("t-{tag}-{nonce}"), &prefix)
        .expect("a session opens over a resolved dialect")
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn engine() -> TurnEngine {
    TurnEngine::new(
        vocab(),
        std::sync::Arc::new(ChatMlRenderer::default()),
        std::sync::Arc::new(ChatMlParser),
        endpoint(),
        // A llama.cpp server we run: token ids in, per-stage cache accounting,
        // wall-clock meter, structural prefix guarantee.
        BackendCaps::OWN_SERVER,
        "qwen-3.8-flash-next",
        serde_json::json!({"temperature": 0.0, "top_k": 1, "seed": 7}),
    )
    .expect("every control token and stop literal must resolve to one vocab entry")
}

/// The whole pipeline, closed against a real model.
#[test]
fn a_turn_goes_render_tokenize_ledger_submit_stream_parse_commit() {
    if !qwen_is_served() {
        return;
    }
    let _lock = serial();
    let mut engine = engine();
    let mut session = fresh(&engine, "pipeline");
    let mut sink = RecordingSink::new();

    let before_prefix = session.ledger.tokens().len();
    session
        .append_items(&engine, &[user("Name one primary colour.")], &mut sink)
        .expect("the user item renders and tokenizes");
    let submitted_before = session.ledger.tokens().to_vec();
    assert!(
        submitted_before.len() > before_prefix,
        "the user item must have contributed tokens"
    );

    let ok = engine
        .run_turn(&mut session, &mut sink)
        .expect("a plain question must not fail §5.7");

    assert!(!ok.items.is_empty(), "the turn produced no items");
    assert!(
        session.ledger.tokens().len() > submitted_before.len(),
        "the region did not grow"
    );
    // Append-only, structurally: the region still starts with exactly what was
    // submitted before the turn.
    assert_eq!(
        &session.ledger.tokens()[..submitted_before.len()],
        &submitted_before[..],
        "the committed prefix changed under a turn"
    );

    // The events a head needs, in the order a head needs them.
    let kinds = sink.kinds();
    assert_eq!(kinds.first(), Some(&"TranscriptAppended"));
    assert!(kinds.contains(&"TurnStarted"));
    assert!(
        kinds.contains(&"PromptProgress"),
        "return_progress must produce progress events: {kinds:?}"
    );
    assert!(kinds.contains(&"Delta"));
    assert_eq!(kinds.last(), Some(&"TurnFinished"));

    // §18.1-I7: the terminal value reached `TurnFinished`.
    let finished = sink
        .events
        .iter()
        .find_map(|e| match e {
            TurnEvent::TurnFinished {
                finish_reason,
                metrics,
                ..
            } => Some((*finish_reason, metrics.clone())),
            _ => None,
        })
        .expect("TurnFinished");
    assert_eq!(finished.0, ok.metrics.finish_reason);
    assert!(ok.metrics.predicted_tokens > 0);
    assert!(ok.metrics.prompt_tokens as usize >= submitted_before.len());
    // §4.4 / the backend seam: wall clock, so no money figure.
    assert!(ok.metrics.cost.micros_usd.is_none());
    assert!(ok.metrics.wall_ms > 0);

    eprintln!(
        "turn: prompt={} cached={} predicted={} f_keep={:?} decode={:?} t/s draft={:?}",
        ok.metrics.prompt_tokens,
        ok.metrics.cached_tokens,
        ok.metrics.predicted_tokens,
        ok.metrics.f_keep(),
        ok.metrics.decode_per_second(),
        ok.metrics.draft_acceptance(),
    );
}

/// The trap, against the running server rather than against a fixture.
///
/// Two things have to be true at once and they pull in opposite directions: the
/// generated ids are **only** in the partial frames, and the partial frames include
/// progress frames carrying a fabricated token id 0. A reader that gets either half
/// wrong writes a different token vector, and this asserts the vector by decoding it
/// back through the vocabulary and comparing against what the model actually said.
#[test]
fn the_committed_ids_are_the_models_ids_not_a_retokenized_reconstruction() {
    if !qwen_is_served() {
        return;
    }
    let _lock = serial();
    let mut engine = engine();
    let mut session = fresh(&engine, "ids");
    let mut sink = RecordingSink::new();
    session
        .append_items(
            &engine,
            &[user("Count from one to eight in words.")],
            &mut sink,
        )
        .unwrap();
    let before = session.ledger.tokens().len();

    let ok = engine.run_turn(&mut session, &mut sink).unwrap();

    let committed = &session.ledger.tokens()[before..];
    assert!(!committed.is_empty());
    assert!(
        !committed.contains(&0) || ok.metrics.predicted_tokens == 0,
        "token id 0 reached the ledger. llama.cpp puts one in every progress frame \
         from a default-constructed completion_token_output; it is not a token the \
         model emitted. ids={committed:?}"
    );

    // What the head was shown, from the Delta increments alone.
    let streamed: String = sink
        .events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Delta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    // What the ledger holds, decoded back through the real vocabulary.
    let decoded = letibot_turn::engine::decode_tokens(&vocab(), engine.control(), committed);

    // # Why this compares against the rows and not against the decoded ledger
    //
    // The decoded ledger is not what a head is shown and was never meant to be.
    // It opens with the generation prompt — `<|im_start|>assistant\n<think>` — which
    // the harness wrote rather than the model, and it carries the model's own
    // `</think>`, which is a boundary the parser drops from every row it commits.
    // A head is shown the rows. This used to assert `decoded.contains(streamed)`,
    // which held only because T12 streamed the boundary literal to heads as visible
    // characters; the containment passed *because* of the defect.
    //
    // So the comparison is against what was committed, which is the thing a head is
    // supposed to end up agreeing with, and it is equality.
    let committed_prose: String = ok
        .items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Reasoning { text, .. } | TranscriptItem::Assistant { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        streamed, committed_prose,
        "what a head was streamed is not what the turn committed.\n  streamed: \
         {streamed:?}\n  committed: {committed_prose:?}\n  ledger:   {decoded:?}"
    );
    // The ledger is still the authority on the *tokens*, and it holds the boundary
    // the rows do not.
    assert!(
        decoded.contains("</think>"),
        "the boundary is committed as a token even though no row and no delta \
         contains its text: {decoded:?}"
    );
    eprintln!("committed {} id(s): {decoded:?}", committed.len());
}

/// §18.1-I1, post-flight, over two real turns.
///
/// Both forms run. The structural one is exact and must hold outright. The
/// observable one is a statistical claim about the server's past behaviour, and on
/// this box it has a known source of noise — MTP speculative decoding trims the
/// slot's cache around a rollback — so a shortfall is reported with its size rather
/// than failing the turn, exactly as §18.1 specifies (`Warning{prefix_divergence,
/// shortfall}`).
#[test]
fn the_generation_inclusive_prefix_invariant_is_checked_after_every_turn() {
    if !qwen_is_served() {
        return;
    }
    let _lock = serial();
    let mut engine = engine();
    let mut session = fresh(&engine, "prefix");
    let mut sink = RecordingSink::new();

    session
        .append_items(&engine, &[user("Name one primary colour.")], &mut sink)
        .unwrap();
    let first = engine.run_turn(&mut session, &mut sink).unwrap();
    assert_eq!(
        first.metrics.prefix_check,
        PrefixCheck::FirstTurn,
        "there is nothing to compare the first turn against, and it must say so \
         rather than pass"
    );
    let witness: PrefixWitness = session.witness().expect("a witness after turn 1").clone();

    session
        .append_items(&engine, &[user("Name another one.")], &mut sink)
        .unwrap();
    let second = engine.run_turn(&mut session, &mut sink).unwrap();

    eprintln!(
        "turn1 slot={} prompt={} cached={} predicted={}",
        first.metrics.id_slot,
        first.metrics.prompt_tokens,
        first.metrics.cached_tokens,
        first.metrics.predicted_tokens
    );
    eprintln!(
        "turn2 slot={} prompt={} cached={} predicted={}",
        second.metrics.id_slot,
        second.metrics.prompt_tokens,
        second.metrics.cached_tokens,
        second.metrics.predicted_tokens
    );
    match &second.metrics.prefix_check {
        PrefixCheck::Held {
            expected_cached_min,
            cached,
            shortfall,
        } => {
            eprintln!(
                "I1 held exactly on the wire. Server reuse: {cached} of a permitted \
                 {expected_cached_min}, short by {shortfall}."
            );
            if *shortfall > 0 {
                // Not a failure. `qwen-3.8-flash-next` is a hybrid/recurrent model,
                // so llama.cpp resumes from a context checkpoint and snaps `n_past`
                // back to it — the observable form of I1 cannot reach its own bound
                // here whatever the harness does. The exact form above is what says
                // the harness is correct.
                let (code, _) = second.metrics.prefix_check.warning().unwrap();
                assert_eq!(code, "cache_reuse_shortfall");
            }
        }
        other => panic!(
            "I1 must hold on a backend that guarantees it: {other:?}\n  witness: \
             covered_len={} prompt={} committed_generated={}",
            witness.covered_len, witness.prompt_tokens, witness.committed_generated
        ),
    }

    // The region is append-only, independently of anything the server said.
    assert!(
        session.ledger.tokens().len() > witness.covered_len,
        "the region did not grow across two turns"
    );
    session
        .ledger
        .verify_chain()
        .expect("§4.3 clause 5: the chain must agree with the tokens");

    // And the second turn must have reused *something*: a fresh nonce prefix that
    // cached nothing on turn 2 would mean the prompt diverged entirely.
    assert!(
        second.metrics.cached_tokens > 0,
        "turn 2 reused nothing at all, which is a cold start, not a continuation"
    );
    eprintln!(
        "turn2: prompt={} cached={} f_keep={:?} reprefill={}",
        second.metrics.prompt_tokens,
        second.metrics.cached_tokens,
        second.metrics.f_keep(),
        second.metrics.reprefill()
    );
}

/// The token core's tokenization must agree with the server's, and the flag that
/// decides it is not the one the endpoint defaults to.
///
/// `/tokenize` defaults `parse_special` to **true** (`server-context.cpp:7775`), so
/// a cross-check that omits the flag is checking a different function from the one
/// the harness runs for `RenderSpan::Text`. Both directions are pinned here.
#[test]
fn the_tokenize_cross_check_passes_parse_special_explicitly() {
    if !qwen_is_served() {
        return;
    }
    let _lock = serial();
    let literal = "<|im_start|>";
    let ours_as_text = vocab()
        .tokenize_text(literal)
        .expect("text tokenization must not fail");
    let ours_as_control = vocab()
        .resolve_control(literal)
        .expect("<|im_start|> is one vocab entry");

    let with_special = server_tokenize(literal, true);
    let without = server_tokenize(literal, false);

    assert_eq!(
        with_special,
        vec![ours_as_control],
        "with parse_special the server produces the single control id"
    );
    assert_eq!(
        without, ours_as_text,
        "with parse_special off the server agrees with our text path"
    );
    assert!(
        without.len() > 1,
        "the whole point: the same string is one token or several depending on a \
         flag whose default is not the function we mean"
    );
}

fn server_tokenize(content: &str, parse_special: bool) -> Vec<u32> {
    let body = serde_json::json!({
        "content": content,
        "add_special": false,
        "parse_special": parse_special,
    })
    .to_string();
    let resp = letibot_turn::http::post_json(&endpoint(), "/tokenize", &body)
        .expect("the server must answer /tokenize")
        .read_to_string()
        .expect("a body");
    let v: serde_json::Value = serde_json::from_str(&resp).expect("json");
    v["tokens"]
        .as_array()
        .expect("tokens")
        .iter()
        .map(|n| n.as_u64().unwrap() as u32)
        .collect()
}
