//! W6 → W7 → W8, closed against the model server that is running on this box.
//!
//! `late_head.rs` proves the mechanics against a synthetic producer, which is where
//! a race is actually catchable — it can publish two thousand events in a
//! millisecond. What it cannot prove is that the **engine's** event stream survives
//! the trip: real delta sizes, real progress frames, `TranscriptAppended` arriving
//! before `TurnFinished`, and a `TurnMetrics` that has to fit through §4.5's
//! `usage`. That is this file.
//!
//! The assertion is the same one, and it is the sharp one: a head that attaches
//! **while the turn is generating** reconstructs, byte for byte, what a head that
//! was there from the start saw.
//!
//! # Why this is not skipped when the server is absent
//!
//! The same reason `live_qwen.rs` is not: a test that quietly passes when the thing
//! it tests is missing reports the health of a `TcpStream::connect`. Set
//! `LETIBOT_COMPLETION_URL` and `LETIBOT_VOCAB_GGUF` if this box is not yours.
//!
//! # The constraint it respects
//!
//! One short generation, one slot, once. The server is in production use.

#[path = "../../turn/tests/support/chatml.rs"]
mod chatml;

use std::sync::OnceLock;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use letibot_backend::BackendCaps;
use letibot_dialect::StablePrefix;
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::{DeltaTarget, SessionEvent};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::lift::LogSink;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::serve;
use letibot_sessionlog::view::TurnState;
use letibot_tokencore::Vocab;
use letibot_transcript::{TranscriptItem, UserPart};
use letibot_turn::{Endpoint, TurnEngine};

use chatml::{ChatMlParser, ChatMlRenderer};

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

/// Is a ChatML model actually behind the port? See `live_qwen.rs` for the whole
/// argument: this test builds a `ChatMlRenderer` and a `ChatMlParser`, so against
/// GLM its control tokens do not resolve and nothing is exercised. Which
/// singleton is up is a fact about the box, so it SKIPS — loudly, saying it is
/// not a pass — rather than leaving the suite permanently red.
fn qwen_is_served() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let want =
            std::env::var("LETIBOT_MODEL_ALIAS").unwrap_or_else(|_| "qwen-3.8-flash-next".into());
        match letibot_turn::serving::served_model(&endpoint()) {
            Ok(served) if letibot_turn::serving::matches(&served, &want) => true,
            Ok(served) => {
                eprintln!(
                    "SKIPPED: {} is serving `{served}`, and this test renders ChatML for \
                     `{want}`, so nothing was run and THIS IS NOT A PASS.",
                    endpoint().authority()
                );
                false
            }
            Err(e) => {
                eprintln!("SKIPPED: could not ask /props ({e}); THIS IS NOT A PASS.");
                false
            }
        }
    })
}

/// What one head saw, accumulated from increments only.
#[derive(Default, Debug, PartialEq)]
struct Seen {
    text: String,
    reasoning: String,
    raw_calls: String,
}

impl Seen {
    fn feed(&mut self, e: &SessionEvent) {
        if let SessionEvent::Delta { target, text, .. } = e {
            match target {
                DeltaTarget::Text => self.text.push_str(text),
                DeltaTarget::Reasoning => self.reasoning.push_str(text),
                DeltaTarget::ToolCall => self.raw_calls.push_str(text),
            }
        }
    }
}

#[test]
fn a_head_attaching_mid_generation_reconstructs_the_turn_exactly() {
    if !qwen_is_served() {
        return;
    }
    // Before a socket, a head or a thread exists. The turn runs on a spawned thread,
    // so a preflight that fires in there panics one thread while this one sits out
    // its 120 s recv timeout and then reports "head A must receive the stream" — a
    // timeout standing in for a model that was never going to answer. Asking here
    // costs one GET and makes the refusal the first thing that happens.
    let _ = endpoint();
    let hub = Hub::new("live");
    let sock = std::env::temp_dir().join(format!(
        "letibot-live-{}-{}.sock",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let server = serve(hub.clone(), &sock).expect("bind");

    // Head A is there from the start.
    let (mut a_client, a_hello, a_reader) =
        HeadClient::attach(&sock, "live", 0, "tui", "early", Caps::default()).expect("attach A");
    let (a_tx, a_rx) = channel();
    let a_pump = std::thread::spawn(move || pump(a_reader, a_tx));
    assert!(matches!(a_hello, ServerFrame::Hello { .. }));

    // The turn runs on its own thread, publishing through the §4.5 seam.
    let hub_for_turn = hub.clone();
    let turn = std::thread::spawn(move || {
        let renderer = ChatMlRenderer::default();
        let parser = ChatMlParser;
        let mut engine = TurnEngine::new(
            vocab(),
            std::sync::Arc::new(renderer),
            std::sync::Arc::new(parser),
            endpoint(),
            BackendCaps::OWN_SERVER,
            "qwen-3.8-flash-next",
            serde_json::json!({"temperature": 0.0, "top_k": 1, "seed": 7}),
        )
        .expect("the dialect must resolve against the vocabulary");

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let prefix = StablePrefix {
            system: format!("You are terse. Session live-head-{nonce}."),
            tools_json: vec![],
        };
        let mut session = engine.open(&format!("t-live-{nonce}"), &prefix).unwrap();
        let mut sink = LogSink::new(hub_for_turn.clone());

        let item = TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "Name the seven days of the week, one per line, nothing else.".into(),
            }],
        };
        session
            .append_items(&engine, std::slice::from_ref(&item), &mut sink)
            .expect("the user item appends");
        // §4.5's TranscriptAppended has no content, so the daemon reconciles it.
        hub_for_turn.record_item(&format!("{}.0", session.transcript_id), item);

        let ok = engine
            .run_turn(&mut session, &mut sink)
            .expect("the turn runs");
        for (i, item) in ok.items.iter().enumerate() {
            hub_for_turn.record_item(
                &format!("{}.{}", session.transcript_id, i + 1),
                item.clone(),
            );
        }
        // Anything the engine could not say through §4.5 goes here, so the test can
        // compare the head's view against the authoritative one.
        (
            ok.items.clone(),
            ok.metrics.prompt_tokens,
            ok.metrics.cached_tokens,
            ok.metrics.predicted_tokens,
        )
    });

    // Head A waits for the first delta, which is what makes the next attach
    // provably mid-generation rather than probably.
    let mut a_seen = Seen::default();
    let mut a_last_seq = 0;
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        let f = a_rx
            .recv_timeout(Duration::from_secs(120))
            .map(Inbound::frame)
            .expect("head A must receive the stream");
        if let ServerFrame::Event(env) = &f {
            a_last_seq = env.seq;
            a_seen.feed(&env.event);
            if matches!(env.event, SessionEvent::Delta { .. }) {
                break;
            }
        }
    }
    assert!(
        !a_seen.text.is_empty() || !a_seen.reasoning.is_empty(),
        "no delta arrived; the engine did not stream"
    );

    // Head B attaches now, mid-generation.
    let (mut b_client, b_hello, b_reader) =
        HeadClient::attach(&sock, "live", 0, "tui", "late", Caps::default()).expect("attach B");
    let (b_tx, b_rx) = channel();
    let b_pump = std::thread::spawn(move || pump(b_reader, b_tx));

    let ServerFrame::Hello {
        snapshot, dropped, ..
    } = b_hello
    else {
        panic!("B must be greeted with a snapshot")
    };
    assert_eq!(dropped, 0);
    let snap = snapshot.expect("a since_seq of 0 gets a snapshot");
    let snap_turn = snap.turn.expect("a turn is in flight");
    assert_eq!(
        snap_turn.state,
        TurnState::Running,
        "B attached after the turn had already ended; the test proved nothing"
    );
    // The accumulated text, **once**. Not a replay of N deltas.
    let mut b_seen = Seen {
        text: snap_turn.text,
        reasoning: snap_turn.reasoning,
        raw_calls: snap_turn.raw_calls,
    };
    let b_from = snap.seq;

    // Drain both heads to the end of the turn.
    let drain = |rx: &std::sync::mpsc::Receiver<Inbound>, seen: &mut Seen| -> u64 {
        let mut last = 0;
        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            let Ok(f) = rx
                .recv_timeout(Duration::from_secs(120))
                .map(Inbound::frame)
            else {
                break;
            };
            if let ServerFrame::Event(env) = &f {
                last = env.seq;
                seen.feed(&env.event);
                if matches!(env.event, SessionEvent::TurnFinished { .. }) {
                    break;
                }
            }
        }
        last
    };
    let a_end = drain(&a_rx, &mut a_seen).max(a_last_seq);
    let b_end = drain(&b_rx, &mut b_seen);

    let (items, prompt_tokens, cached_tokens, predicted_tokens) = turn.join().expect("the turn");

    // The point of the whole file.
    assert_eq!(
        b_seen, a_seen,
        "the late head's snapshot-plus-increments did not equal the early head's stream"
    );
    assert!(!a_seen.text.is_empty(), "the model produced no text");
    assert!(b_end >= b_from, "B received nothing after its snapshot");
    assert!(a_end > 0);

    // And the head's live view agrees with what was committed to the transcript —
    // about the **channel**, not merely about the characters. This is T12.
    //
    // # What this used to say, and why it now says more
    //
    // Measured here first: the *content* agreed — every committed character
    // reached a head — while the **channel** did not. `DeltaTarget` said `Text`
    // for the whole generation, including the reasoning block and the literal
    // `</think>`, while the parser committed a `Reasoning` item and an `Assistant`
    // item. So for the length of every turn a head showed the model's reasoning as
    // its answer, and the transcript row then replaced it. Two causes in
    // `crates/turn`, both now fixed: `in_reasoning` was seeded `false` rather than
    // from the dialect's generation prompt, which ends inside `<think>`; and a
    // chunk's text was emitted as a `Delta` before the ids in that chunk had their
    // roles inspected, so a boundary's literal arrived as visible characters.
    //
    // # Why per-channel equality is the right assertion and is about the whole turn
    //
    // `Seen` only ever appends, and it appends into the accumulator the delta
    // named. A delta announced on the wrong channel therefore lands in the wrong
    // string and no later delta can move it: there is no way to pass this by
    // being right only at the end, which was exactly the failure mode. Equality
    // rather than containment, because the transcript is the authority on what the
    // turn said and a head that shows a character the row does not contain — a
    // boundary literal, say — is as wrong as one that drops a character it does.
    //
    // # What it cannot do
    //
    // It cannot tell the two causes apart: either one alone leaves a visible
    // artefact and fails here. `crates/turn/tests/engine_decisions.rs` separates
    // them against canned frames, where the seed and the emission order can be
    // regressed one at a time.
    let committed_assistant: String = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let committed_reasoning: String = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Reasoning { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();

    // A run in which the model never reasoned would satisfy everything below
    // vacuously, and the defect this file found lives entirely in the reasoning
    // channel. The generation prompt hands the model an open `<think>`, so a turn
    // with no reasoning item at all means the run did not exercise the split —
    // report that as such rather than passing.
    assert!(
        !committed_reasoning.is_empty(),
        "the turn committed no reasoning item, so the channel split was never \
         exercised. This run proved nothing about T12; it is not evidence that the \
         engine is correct."
    );
    assert_eq!(
        a_seen.reasoning, committed_reasoning,
        "the head's Reasoning deltas do not equal the committed reasoning row"
    );
    assert_eq!(
        a_seen.text, committed_assistant,
        "the head's Text deltas do not equal the committed assistant row"
    );

    // The second cause again, phrased so that it fails on its own even if the
    // model happened to commit nothing on one of the two channels: a boundary is
    // structure, the parser drops it from every row, and a head that renders it is
    // showing markup the transcript does not contain.
    let everything_a_head_saw = format!("{}{}", a_seen.reasoning, a_seen.text);
    for literal in [
        chatml::THINK_OPEN.literal.as_ref(),
        chatml::THINK_CLOSE.literal.as_ref(),
        chatml::IM_START.literal.as_ref(),
        chatml::IM_END.literal.as_ref(),
    ] {
        assert!(
            !everything_a_head_saw.contains(literal),
            "the control literal {literal:?} reached a head as visible text; no \
             committed row contains it"
        );
    }

    // §4.5's `usage` carries what the head needs and nothing it cannot use.
    let snap = hub.snapshot();
    let Some(TurnState::Finished { usage, .. }) = snap.turn.as_ref().map(|t| t.state.clone())
    else {
        panic!("the turn should be finished in the view")
    };
    assert_eq!(usage.prompt_tokens, prompt_tokens);
    assert_eq!(usage.cached_tokens, cached_tokens);
    assert_eq!(usage.predicted_tokens, predicted_tokens);
    assert!(usage.f_sim().is_some());

    // A third head, attaching after everything: the *replayed* stream is scrubbed,
    // and the progress frames the live heads saw are not in it.
    let (mut c_client, c_hello, c_reader) =
        HeadClient::attach(&sock, "live", 1, "tui", "after", Caps::default()).expect("attach C");
    let (c_tx, c_rx) = channel();
    let c_pump = std::thread::spawn(move || pump(c_reader, c_tx));
    let ServerFrame::Hello { scrubbed, .. } = c_hello else {
        panic!()
    };
    assert!(
        scrubbed.prompt_progress > 0,
        "the engine emits progress frames and a replay must strip them"
    );
    let mut saw_progress = false;
    while let Ok(f) = c_rx
        .recv_timeout(Duration::from_millis(500))
        .map(Inbound::frame)
    {
        if let ServerFrame::Event(env) = f
            && matches!(env.event, SessionEvent::PromptProgress { .. })
        {
            saw_progress = true;
        }
    }
    assert!(
        !saw_progress,
        "a replayed progress frame is a lie about now"
    );

    eprintln!(
        "live e2e: {} chars of text, {} of reasoning; prompt={prompt_tokens} \
         cached={cached_tokens} predicted={predicted_tokens}; \
         B attached at seq {b_from}, {} progress frames stripped on replay",
        a_seen.text.len(),
        a_seen.reasoning.len(),
        scrubbed.prompt_progress
    );

    let _ = a_client.detach();
    let _ = b_client.detach();
    let _ = c_client.detach();
    drop(a_client);
    drop(b_client);
    drop(c_client);
    let _ = a_pump.join();
    let _ = b_pump.join();
    let _ = c_pump.join();
    server.shutdown();
}
