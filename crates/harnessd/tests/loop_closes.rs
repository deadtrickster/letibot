//! The loop, closed against the model server that is running on this box.
//!
//! # Why this is not skipped when the server is absent
//!
//! The same reason `letibot-turn`'s and `letibot-sessionlog`'s live tests are not:
//! a test that quietly passes when the thing it tests is missing reports the health
//! of a `TcpStream::connect`, and this fleet has already paid for the difference
//! between a liveness indicator and the fact it stands in for. If this box is not
//! yours, set `LETIBOT_COMPLETION_URL`, `LETIBOT_VOCAB_GGUF` and
//! `LETIBOT_MODEL_ALIAS`.
//!
//! # The constraint it respects
//!
//! The server is in production use with five shared slots. This file is **two short
//! generations**, under one process-wide lock. `letibot-m1` is where the long
//! measurement lives, and it is a binary you run rather than a test that runs
//! itself.
//!
//! # What is asserted, and why each one is here
//!
//! 1. **The loop closes.** A prompt that needs a file produces a tool call, the
//!    call runs, its result is appended, and the model answers from it. That is the
//!    whole of T17 in one assertion.
//! 2. **A head can rebuild the conversation.** §4.5's `TranscriptAppended` carries
//!    no content (T13.1), so the daemon reconciles out of band through
//!    `Hub::record_item`. If the ids ever stop matching, the snapshot goes empty
//!    and this fails — which is the only way that regression is visible, since
//!    nothing else in the workspace pairs the two id-minting rules.
//! 3. **A head drives it over the socket.** Attach, prompt, watch the deltas, see
//!    the turn finish. That is the daemon rather than the loop, and it is the path
//!    `letibot-tui` takes; `letibot-m1` never touches it, so nothing else covers it.
//! 4. **An inert tool does not become a success.** `ask_corpus` has nothing behind
//!    it and must say so structurally — in a marked envelope in the transcript, not
//!    in prose the model can paraphrase away.

use std::sync::{Mutex, MutexGuard, OnceLock};

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_transcript::{ToolOutcome, TranscriptItem};
use letibot_turn::Endpoint;

/// One at a time: this box's slots are shared with real work.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn config() -> Config {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();
    let mut cfg = Config::for_this_box(repo);
    cfg.socket = std::env::temp_dir().join(format!(
        "letibot-loop-{}-{}.sock",
        std::process::id(),
        letibot_harnessd::config::now_ns()
    ));
    if let Ok(url) = std::env::var("LETIBOT_COMPLETION_URL") {
        let (h, p) = url.rsplit_once(':').expect("HOST:PORT");
        cfg.endpoint = Endpoint::new(h, p.parse().expect("port"));
    }
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = g.into();
    }
    if let Ok(m) = std::env::var("LETIBOT_MODEL_ALIAS") {
        cfg.model = m;
    }
    cfg.dialect = Dialect::Qwen;
    // Short answers, and a bound low enough that a model which loops is a failing
    // test rather than a slow one.
    cfg.effort = Some("low".into());
    cfg.max_tool_rounds = 5;
    cfg
}

#[test]
fn a_tool_call_goes_out_runs_and_comes_back_as_an_answer() {
    let _lock = serial();
    let cfg = config();
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new("loop-test");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");

    let reply = h
        .submit(
            "Read the file Cargo.toml at the workspace root and reply with the value of \
             `resolver`. Reply with just the value in quotes, nothing else.",
        )
        .expect("the loop must close");

    assert!(
        reply.tool_calls >= 1,
        "the model answered without reading the file, so this measured nothing: {:?}",
        reply.text
    );
    assert!(reply.rounds >= 2, "a tool call costs at least two submissions");
    assert!(
        reply.text.contains('3'),
        "the answer did not come from the file it read: {:?}",
        reply.text
    );

    // The tool result really is in the transcript, as a row, with its outcome.
    let results: Vec<&TranscriptItem> = h
        .items()
        .iter()
        .filter(|i| matches!(i, TranscriptItem::ToolResult { .. }))
        .collect();
    assert!(!results.is_empty(), "no ToolResult row was appended");

    // §4.3: every item owns tokens, and the chain still agrees with them.
    for (i, item) in h.items().iter().enumerate() {
        if matches!(item, TranscriptItem::SegmentMark { .. }) {
            continue;
        }
        assert!(h.row_len(i) > 0, "item {i} ({item:?}) owns no tokens");
    }

    // T13.1's gap, closed out of band: a head that attaches now must see the
    // conversation, not a list of empty rows.
    let snap = hub.snapshot();
    assert!(
        !snap.items.is_empty(),
        "the snapshot is empty; Hub::record_item is not wired to the ids the log announced"
    );
    let rendered = format!("{:?}", snap.items);
    assert!(
        rendered.contains("Cargo.toml"),
        "the head's snapshot does not contain the user's message; the item ids the \
         daemon recorded do not match the ones TranscriptAppended carried"
    );
}

#[test]
fn an_inert_retrieval_tool_reports_not_run_and_says_so_structurally() {
    let _lock = serial();
    let cfg = config();
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new("abstain-test");
    let mut h = Harness::open(&parts, cfg, hub).expect("the session opens");

    let reply = h
        .submit("Use ask_corpus to look up our deployment policy, then tell me what it said.")
        .expect("the loop must close");
    assert!(reply.tool_calls >= 1, "nothing was asked: {:?}", reply.text);

    // `NotRun`, **not** `Abstained`, and the difference is the whole argument for
    // leaving retrieval inert rather than stubbing it. `Abstained` means the corpus
    // was searched and does not cover this; `NotRun` means nothing was searched.
    // Reporting the second as the first would be a claim about a corpus nobody
    // queried — which is one step from the failure §8.2 exists to prevent.
    let outcomes: Vec<ToolOutcome> = h
        .items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::ToolResult { name, outcome, .. } if name.starts_with("ask_") => {
                Some(outcome.clone())
            }
            _ => None,
        })
        .collect();
    assert!(!outcomes.is_empty(), "no retrieval tool ran");
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, ToolOutcome::NotRun { .. })),
        "a retrieval tool with nothing behind it returned {outcomes:?}. It must be \
         NotRun: nothing ran, which is a different fact from `the corpus does not \
         cover this`, and both are different from an answer."
    );
    assert!(
        !outcomes.iter().any(|o| matches!(o, ToolOutcome::Ok)),
        "an inert retrieval tool reported success"
    );

    // And the model was told structurally, not in prose it could paraphrase away.
    let payloads: Vec<String> = h
        .items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::ToolResult { name, payload, .. } if name.starts_with("ask_") => {
                Some(payload.clone())
            }
            _ => None,
        })
        .collect();
    // The class is carried by the envelope the runtime chose, not by the wording:
    // `Abstained` gets `NO_RESULT`, everything else that is not `Ok` gets
    // `TOOL_ERROR`. Classified rather than string-matched, so a re-wording of the
    // body cannot make this pass or fail.
    for p in &payloads {
        assert_eq!(
            letibot_tools::Envelope::classify(p),
            Some("TOOL_ERROR"),
            "the result reached the model with no recognisable envelope: {p:?}"
        );
    }
    // And exactly one envelope. The tool runtime renders it and the dialect used to
    // render a second one around it; a result wrapped twice is what T17 found by
    // running the two crates together for the first time.
    for p in &payloads {
        assert_eq!(
            p.matches("<<<").count(),
            2,
            "the result carries more than one envelope: {p:?}"
        );
    }
}


/// The daemon, driven the way a person drives it: over the Unix socket.
///
/// `letibot-m1` calls `Harness::submit` directly, so it proves the loop and proves
/// nothing about the socket. This is the other half — attach a head, submit a
/// prompt as a command, and require that the answer comes back as events on the
/// same connection, with the turn's own boundary events around it.
#[test]
fn a_head_prompts_over_the_socket_and_sees_the_turn() {
    use letibot_sessionlog::client::{HeadClient, pump};
    use letibot_sessionlog::event::SessionEvent;
    use letibot_sessionlog::protocol::{Caps, ServerFrame};
    use letibot_harnessd::Daemon;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    let _lock = serial();
    let cfg = config();
    let socket = cfg.socket.clone();
    let session = "socket-test".to_string();
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(session.clone());
    let daemon = Daemon::serve(hub.clone(), &socket).expect("the socket binds");
    let mut harness = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");

    let (mut client, hello, reader) =
        HeadClient::attach(&socket, &session, 0, "tui", "test", Caps::default())
            .expect("a head attaches");
    assert!(matches!(hello, ServerFrame::Hello { .. }));
    let (tx, rx) = channel();
    let pump_thread = std::thread::spawn(move || pump(reader, tx));

    // The **head** goes on the other thread and the worker stays here, which is the
    // opposite of `harnessd`'s arrangement. Not a preference: `ToolRuntime` holds a
    // `Box<dyn Gate>`, and `Gate` — alone among `Tool`, `ExecBackend`, `InlineBudget`
    // and `SpillStore` — is declared without `Send + Sync`, so a `Harness` cannot be
    // moved onto a thread at all. The daemon does not care today (its worker runs on
    // the main thread), and §13.2's multi-head daemon will. Recorded here rather than
    // fixed, because `Gate` is the seam W11 is supposed to absorb and constraining it
    // from a test is not this strand's call.
    let hub_for_head = hub.clone();
    let head = std::thread::spawn(move || {
        client
            .prompt(0, "Reply with exactly the word: pong")
            .expect("the command is accepted");

        let mut text = String::new();
        let mut finished = false;
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline && !finished {
            let Ok(frame) = rx.recv_timeout(Duration::from_secs(120)) else {
                break;
            };
            if let ServerFrame::Event(env) = frame {
                match env.event {
                    SessionEvent::Delta { text: ref t, .. } => text.push_str(t),
                    SessionEvent::TurnFinished { .. } => finished = true,
                    _ => {}
                }
            }
        }
        // Closing the hub is what ends `Daemon::run`, which is exactly what a
        // SIGINT does in the real daemon.
        hub_for_head.close();
        (text, finished)
    });

    daemon.run(&mut harness, |_, _| {});
    let (text, finished) = head.join().expect("the head thread");
    daemon.shutdown();
    let _ = pump_thread.join();

    assert!(finished, "no TurnFinished reached the head; got {text:?}");
    assert!(
        text.to_lowercase().contains("pong"),
        "the head saw no answer, only {text:?}"
    );
}
