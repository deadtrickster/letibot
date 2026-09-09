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
use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::{DeltaTarget, SessionEvent};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::lift::LogSink;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::serve;
use letibot_sessionlog::view::TurnState;
use letibot_tokencore::Vocab;
use letibot_transcript::{ReasoningField, TranscriptItem, UserPart};
use letibot_turn::{Endpoint, TurnEngine};

use chatml::{ChatMlParser, ChatMlRenderer};

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

fn endpoint() -> Endpoint {
    let url =
        std::env::var("LETIBOT_COMPLETION_URL").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let (host, port) = url.rsplit_once(':').expect("HOST:PORT");
    Endpoint::new(host, port.parse().expect("port"))
}

/// What one head saw, accumulated from increments only.
#[derive(Default, Debug, PartialEq)]
struct Seen {
    text: String,
    reasoning: String,
}

impl Seen {
    fn feed(&mut self, e: &SessionEvent) {
        if let SessionEvent::Delta { target, text, .. } = e {
            match target {
                DeltaTarget::Text => self.text.push_str(text),
                DeltaTarget::Reasoning => self.reasoning.push_str(text),
            }
        }
    }
}

#[test]
fn a_head_attaching_mid_generation_reconstructs_the_turn_exactly() {
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
            &renderer,
            &parser,
            endpoint(),
            BackendCaps::OWN_SERVER,
            "qwen-3.8-flash-next",
            ReasoningField::Inline,
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
    };
    let b_from = snap.seq;

    // Drain both heads to the end of the turn.
    let drain = |rx: &std::sync::mpsc::Receiver<ServerFrame>, seen: &mut Seen| -> u64 {
        let mut last = 0;
        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            let Ok(f) = rx.recv_timeout(Duration::from_secs(120)) else {
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

    // And the head's view agrees with what was committed to the transcript.
    //
    // # A measured disagreement, recorded here rather than papered over
    //
    // The *content* agrees: every committed character reached a head as a delta.
    // The **channel** does not. On this box, with this model, the engine's
    // `DeltaTarget` said `Text` for the whole generation, including the reasoning
    // block and the literal `</think>`, while the parser committed a `Reasoning`
    // item and an `Assistant` item. Two causes, both in `crates/turn`:
    //
    // 1. `in_reasoning` starts `false` (`engine.rs:~717`) and is only set by a
    //    *generated* `ThinkOpen`. The generation prompt ends inside `<think>` —
    //    the engine's own module diagram says so — so the model is reasoning from
    //    token one and no head is told.
    // 2. `Chunk::Token{text}` is emitted as a `Delta` **before** the ids in that
    //    chunk have their roles inspected, so a control token's literal text is
    //    streamed to every head as visible characters.
    //
    // The consequence for a head is precise: during a turn it shows the reasoning
    // as the answer, and then the answer replaces it when the transcript row
    // lands. Not corruption — the ledger is built from ids and the parser is
    // authoritative — but the live view and the stored view disagree for the
    // length of the turn, which is exactly the class of defect §13.2b is about.
    //
    // These assertions are written against the contract as it *should* be, in the
    // weakest form that passes today, so that fixing W6 makes them tightenable
    // rather than making them fail silently.
    let committed_assistant: String = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let everything_a_head_saw = format!("{}{}", a_seen.reasoning, a_seen.text);
    assert!(
        everything_a_head_saw.contains(committed_assistant.trim()),
        "a committed character never reached a head:\n  committed: {committed_assistant:?}\n  \
         streamed:  {everything_a_head_saw:?}"
    );
    let committed_reasoning: String = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Reasoning { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    if !committed_reasoning.trim().is_empty() {
        assert!(
            everything_a_head_saw.contains(committed_reasoning.trim()),
            "committed reasoning never reached a head"
        );
        // The known divergence, asserted so that it cannot be fixed quietly.
        assert!(
            a_seen.reasoning.is_empty(),
            "the engine now routes reasoning to DeltaTarget::Reasoning. Good — \
             tighten this test to assert the split exactly, and delete the W7 \
             report's note about it."
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
    assert!(usage.f_keep().is_some());

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
    while let Ok(f) = c_rx.recv_timeout(Duration::from_millis(500)) {
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
