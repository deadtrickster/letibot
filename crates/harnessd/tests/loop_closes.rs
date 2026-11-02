//! The loop, closed against the model server that is running on this box. These are live
//! tests: not skipped when the server is absent, because a test that quietly passes when
//! the thing it tests is missing reports the health of a `TcpStream::connect`. If this box
//! is not yours, set `LETIBOT_COMPLETION_URL`, `LETIBOT_VOCAB_GGUF` and
//! `LETIBOT_MODEL_ALIAS`. The server is in production use with five shared slots, so this
//! file is two short generations under one process-wide lock; the long measurement lives in
//! `letibot-m1`, a binary you run. What is asserted: a prompt that needs a file produces a
//! tool call, the call runs, its result is appended and the model answers from it

use std::sync::{Mutex, MutexGuard, OnceLock};

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_transcript::{ToolOutcome, TranscriptItem};
use letibot_turn::Endpoint;

mod support;

/// One at a time: this box's slots are shared with real work.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The models this box can be serving, by the path `/props` reports: the alias, the dialect
/// that pairs with it, and the GGUF its vocabulary comes from. A table rather than a guess
fn known_local_model(served: &str) -> Option<(&'static str, Dialect, &'static str)> {
    const KNOWN: &[(&str, &str, Dialect, &str)] = &[
        (
            "GLM-5.3-Flash",
            "glm-5.3-flash",
            Dialect::Glm,
            "/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf",
        ),
        (
            "Qwen3.8-Flash-Next",
            "qwen-3.8-flash-next",
            Dialect::Qwen,
            "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf",
        ),
        (
            "Qwen3.8-27B",
            "qwen3.8-27b",
            Dialect::Qwen,
            "/home/dead/models/Qwen3.8-27B-UD-Q6_K_XL.gguf",
        ),
    ];
    KNOWN.iter().find_map(|(needle, alias, d, gguf)| {
        (served.contains(needle) && std::path::Path::new(gguf).exists())
            .then_some((*alias, *d, *gguf))
    })
}

/// The operator's per-project mode is not this test's business. `Harness::open` applies the
/// project store's row over `cfg.mode`, and that row is whatever was last set with `/mode` - a mode
/// the test never asked for, and a refusal about an authorisation oracle.
fn own_modes(parts: Parts) -> Parts {
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    parts
}

fn config() -> Option<Config> {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();
    let mut cfg = Config::for_this_box(repo);
    cfg.socket = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-loop-{}-{}.sock-{}",
        std::process::id(),
        letibot_harnessd::config::now_ns(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    if let Ok(url) = std::env::var("LETIBOT_COMPLETION_URL") {
        let (h, p) = url.rsplit_once(':').expect("HOST:PORT");
        cfg.endpoint = Endpoint::new(h, p.parse().expect("port"));
    }
    // `Config::for_this_box` carries no vocabulary default any more, so a harness built here is
    // handed one: the same path this file's gate consults.
    cfg.vocab_gguf = support::vocab::gguf();
    // The dialect follows the model, and that is what makes the escape hatch work.
    cfg.dialect = Dialect::Qwen;
    if let Ok(m) = std::env::var("LETIBOT_MODEL_ALIAS") {
        cfg.dialect = Dialect::parse(&m)
            .unwrap_or_else(|| panic!("LETIBOT_MODEL_ALIAS={m} names no dialect this build knows"));
        cfg.model = m;
    }
    // What is actually behind the endpoint, asked before anything is tokenised for it
    if std::env::var("LETIBOT_MODEL_ALIAS").is_err()
        && let Ok(served) = letibot_turn::serving::served_model(&cfg.endpoint)
        && !letibot_turn::serving::matches(&served, &cfg.model)
        && let Some((alias, dialect, gguf)) = known_local_model(&served)
    {
        eprintln!(
            "preflight: {} serves `{served}`, so this test runs against {alias} rather \
             than refusing. LETIBOT_MODEL_ALIAS overrides.",
            cfg.endpoint.authority()
        );
        cfg.model = alias.into();
        cfg.dialect = dialect;
        cfg.vocab_gguf = Some(gguf.into());
    }
    // NO SERVER, NO TEST - and the guard lives in `serving` so its wording cannot drift from
    // the other live files.
    if letibot_turn::serving::skip_live_test(&cfg.endpoint, "This test") {
        return None;
    }
    // `expect` runs once per process, not once per test. It panics when the server at the
    // endpoint is serving a model other than the one named - the failure the escape hatch exists
    // for - and four tests each re-checking it would fetch `/props` four times for one answer.
    static CHECKED: OnceLock<()> = OnceLock::new();
    CHECKED.get_or_init(|| letibot_turn::serving::expect(&cfg.endpoint, &cfg.model));
    // Short answers, and a bound low enough that a model which loops is a failing
    // test rather than a slow one.
    cfg.effort = Some("low".into());
    cfg.max_tool_rounds = 5;
    Some(cfg)
}

#[test]
fn a_tool_call_goes_out_runs_and_comes_back_as_an_answer() {
    let _lock = serial();
    let Some(cfg) = config() else { return };
    let parts = own_modes(Parts::load(&cfg).expect("the vocabulary must load"));
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
    assert!(
        reply.rounds >= 2,
        "a tool call costs at least two submissions"
    );
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
    let Some(cfg) = config() else { return };
    let parts = own_modes(Parts::load(&cfg).expect("the vocabulary must load"));
    let hub = Hub::new("abstain-test");
    let mut h = Harness::open(&parts, cfg, hub).expect("the session opens");

    let reply = h
        .submit("Use ask_corpus to look up our deployment policy, then tell me what it said.")
        .expect("the loop must close");
    assert!(reply.tool_calls >= 1, "nothing was asked: {:?}", reply.text);

    // `NotRun`, not `Abstained`, and the difference is the whole argument for leaving
    // retrieval inert rather than stubbing it. `Abstained` means the corpus was searched and
    // does not cover this; `NotRun` means nothing was searched.
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
    // The class is carried by the envelope the runtime chose, not by the wording: `Abstained` gets
    // `NO_RESULT`, everything else that is not `Ok` gets `TOOL_ERROR`. Classified rather than
    // string-matched, so a re-wording of the body cannot make this pass or fail.
    for p in &payloads {
        assert_eq!(
            letibot_tools::Envelope::classify(p),
            Some("TOOL_ERROR"),
            "the result reached the model with no recognisable envelope: {p:?}"
        );
    }
    // And exactly one envelope. The tool runtime renders it and the dialect used to render a second
    // one around it; a result wrapped twice is what running the two crates together first found.
    for p in &payloads {
        assert_eq!(
            p.matches("<<<").count(),
            2,
            "the result carries more than one envelope: {p:?}"
        );
    }
}

/// The daemon, driven the way a person drives it: over the Unix socket. `letibot-m1` calls
/// `Harness::submit` directly, so it proves the loop and nothing about the socket.
#[test]
fn a_head_prompts_over_the_socket_and_sees_the_turn() {
    use letibot_harnessd::{Daemon, Sessions};
    use letibot_sessionlog::client::{HeadClient, Inbound, pump};
    use letibot_sessionlog::event::SessionEvent;
    use letibot_sessionlog::protocol::{Caps, ServerFrame};
    use letibot_sessionlog::registry::Registry;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    let _lock = serial();
    let Some(mut cfg) = config() else { return };
    let socket = cfg.socket.clone();
    let session = "socket-test".to_string();
    cfg.session_id = session.clone();
    let parts = own_modes(Parts::load(&cfg).expect("the vocabulary must load"));
    let registry = Registry::new();
    registry
        .create(session.clone(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    let mut sessions =
        Sessions::open_first(&parts, cfg, registry.clone()).expect("the session opens");

    let (mut client, hello, reader) =
        HeadClient::attach(&socket, &session, 0, "tui", "test", Caps::default())
            .expect("a head attaches");
    assert!(matches!(hello, ServerFrame::Hello { .. }));
    let (tx, rx) = channel();
    let pump_thread = std::thread::spawn(move || pump(reader, tx));

    // The head goes on the other thread and the worker stays here, which is the opposite of
    // `harnessd`'s arrangement. Not a preference: it is the shape that lets the worker own the
    // harness while a head pumps its own socket.
    let registry_for_head = registry.clone();
    let head = std::thread::spawn(move || {
        client
            .prompt(0, "Reply with exactly the word: pong")
            .expect("the command is accepted");

        let mut text = String::new();
        let mut finished = false;
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline && !finished {
            let Ok(frame) = rx
                .recv_timeout(Duration::from_secs(120))
                .map(Inbound::frame)
            else {
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
        // Closing the registry is what ends `Daemon::run`, which is exactly what a SIGINT does in
        // the real daemon. Closing one *hub* would not: the worker waits on the cross-session bell
        // and a daemon that stopped because one of its sessions did would take the others down.
        registry_for_head.close();
        (text, finished)
    });

    daemon.run(&mut sessions, |_, _| {});
    let (text, finished) = head.join().expect("the head thread");
    daemon.shutdown();
    let _ = pump_thread.join();

    assert!(finished, "no TurnFinished reached the head; got {text:?}");
    assert!(
        text.to_lowercase().contains("pong"),
        "the head saw no answer, only {text:?}"
    );
}

/// A message already waiting joins THIS round rather than costing another.
#[test]
fn a_message_already_waiting_joins_this_round_rather_than_costing_another() {
    use letibot_sessionlog::hub::CommandKind;
    use letibot_sessionlog::protocol::{Caps, ServerFrame};
    use letibot_transcript::{TranscriptItem, UserPart};

    let _lock = serial();
    let Some(cfg) = config() else { return };
    let parts = own_modes(Parts::load(&cfg).expect("the vocabulary must load"));
    let hub = Hub::new("steering-greedy-test");
    let head = hub.attach("tui", "test", Caps::default(), 0);
    // Seeded before the turn: waiting when the round begins, which is the greedy case.
    let frame = hub.submit(
        &head.head_id,
        "queued-1",
        0,
        CommandKind::Prompt {
            text: "This message was queued while you were working on the previous \
                   message. Reply with exactly the word: two"
                .into(),
            attachments: Vec::new(),
        },
    );
    assert!(
        matches!(frame, ServerFrame::Accepted { .. }),
        "the queued command is accepted: {frame:?}"
    );
    let mut h = Harness::open(&parts, cfg, hub).expect("the session opens");
    let reply = h
        .submit("Reply with exactly the word: one")
        .expect("the loop must close");

    // ONE round: the words were already there, so they went into this prompt rather than waiting
    // for a boundary that would have cost another generation.
    assert_eq!(
        reply.rounds, 1,
        "a message already waiting must not cost an extra round; the reply was {reply:?}"
    );

    // And both prompts are in the transcript, with the queued one ahead of anything the model
    // said - the engine's `steering_before`, appended before generation.
    let rows: Vec<String> = h
        .items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { parts, .. } => match &parts[0] {
                UserPart::Text { text } => Some(format!("user: {text}")),
                _ => None,
            },
            TranscriptItem::Assistant { text, .. } => Some(format!("assistant: {text}")),
            _ => None,
        })
        .collect();
    let first = rows
        .iter()
        .position(|r| r.starts_with("user: Reply with exactly"))
        .expect("the first prompt is in the transcript");
    let queued = rows
        .iter()
        .position(|r| r.starts_with("user: This message was queued"))
        .expect("the queued message is in the transcript");
    let answered = rows
        .iter()
        .position(|r| r.starts_with("assistant:") && !r.trim_end().ends_with("assistant:"))
        .expect("the round was answered");
    assert!(
        first < queued && queued < answered,
        "the queued words must be in the prompt ahead of the answer: {rows:?}"
    );
}

/// A message that arrives WHILE the model is generating is answered in the next round. The
/// greedy poll takes what is waiting when the round BEGINS
#[test]
fn a_message_that_arrives_mid_turn_is_answered_in_the_next_round() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use letibot_sessionlog::event::SessionEvent;
    use letibot_sessionlog::hub::CommandKind;
    use letibot_sessionlog::protocol::{Caps, ServerFrame};
    use letibot_transcript::{TranscriptItem, UserPart};

    let _lock = serial();
    let Some(cfg) = config() else { return };
    let parts = own_modes(Parts::load(&cfg).expect("the vocabulary must load"));
    let hub = Hub::new("steering-midturn-test");
    let head = hub.attach("tui", "test", Caps::default(), 0);
    let head_id = head.head_id.clone();

    let hub_for_thread: Arc<Hub> = hub.clone();
    let queued = std::thread::spawn(move || {
        // Wait until the prompt has gone out. A Delta is proof: the round is past the greedy poll
        // so this message can only be absorbed at the boundary.
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut saw_delta = false;
        while Instant::now() < deadline && !saw_delta {
            saw_delta = hub_for_thread
                .retained()
                .iter()
                .any(|e| matches!(e.event, SessionEvent::Delta { .. }));
            if !saw_delta {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let frame = hub_for_thread.submit(
            &head_id,
            "queued-1",
            0,
            CommandKind::Prompt {
                text: "Reply with exactly the word: two".into(),
                attachments: Vec::new(),
            },
        );
        (saw_delta, matches!(frame, ServerFrame::Accepted { .. }))
    });

    let mut h = Harness::open(&parts, cfg, hub).expect("the session opens");
    // Long enough that the thread's submit lands well inside the generation.
    let reply = h
        .submit("Count slowly from 1 to 60, one number per line, and then say the word: one")
        .expect("the loop must close");

    let (saw_delta, accepted) = queued.join().expect("the submitting thread");
    assert!(
        saw_delta,
        "the message never landed mid-turn, so this test proved nothing about the boundary"
    );
    assert!(accepted, "the queued command was refused");

    // TWO rounds: the first answered, the boundary absorbed the queued words, and the loop went
    // round again instead of returning.
    assert_eq!(
        reply.rounds, 2,
        "a message that arrived mid-turn must cost a round; the reply was {reply:?}"
    );

    // And the order says the queued words came after the first answer, not before it - which is the
    // difference between this path and the greedy one above.
    let rows: Vec<String> = h
        .items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { parts, .. } => match &parts[0] {
                UserPart::Text { text } => Some(format!("user: {text}")),
                _ => None,
            },
            TranscriptItem::Assistant { text, .. } => Some(format!("assistant: {text}")),
            _ => None,
        })
        .collect();
    let first_answer = rows
        .iter()
        .position(|r| r.starts_with("assistant:"))
        .expect("the first prompt was answered");
    let queued_at = rows
        .iter()
        .position(|r| r.starts_with("user: Reply with exactly the word: two"))
        .expect("the queued message is in the transcript");
    assert!(
        first_answer < queued_at,
        "the queued words must arrive AFTER the first answer on this path: {rows:?}"
    );
    assert!(
        rows.iter()
            .skip(queued_at + 1)
            .any(|r| r.starts_with("assistant:") && r.to_lowercase().contains("two")),
        "the queued message was appended but never answered: {rows:?}"
    );
}
