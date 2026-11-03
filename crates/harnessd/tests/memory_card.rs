//! The memory card above the ceiling, answered by a real head over the session socket.
//!
//! `AdjudicatedGate::grant_memory` poses the ask a `bash` call makes when it wants more than half of
//! what is free, and the card's path was built and tested against a fake adjudicator - no head has
//! ever been put in front of it. Here the question is posed on one thread and answered on another
//! through `ClientFrame::Answer`, which is the frame the head a person is looking at sends.
//!
//! The ask is built from `/proc/meminfo`, because that is where the numbers come from and an ask no
//! fixture can produce proves nothing about a box; `apparatus::applies` says so on a platform that
//! has no such file, and it is not a refusable skip - an OS that never had the file is not missing
//! apparatus.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use letibot_harnessd::answers::{Answers, HeadAdjudicator};
use letibot_sessionlog::Registry;
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::serve_registry;
use letibot_tools::exec::memory::{
    Ask, Decision, Meminfo, MemoryAsk, MemoryGrant, MemoryPolicy, decide,
};
use letibot_tools::{AdjudicatedGate, Gate};

/// How long a card may take. The gate's own budget is longer, so a wait here that runs out is this
/// fixture failing rather than the product timing out.
const PATIENCE: Duration = Duration::from_secs(20);

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-memory-card-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// The ask, out of the box's own numbers: `uncapped` is the ask the rule never grants without a
/// person, and it is reachable on any box with a memory controller.
fn an_uncapped_ask(mem: &Meminfo) -> MemoryAsk {
    let policy = MemoryPolicy::default();
    let Decision::Operator { bytes, ceiling } = decide(&policy, Ask::Uncapped, mem) else {
        panic!("an uncapped ask is the operator's, whatever the ceiling is");
    };
    assert_eq!(bytes, None, "uncapped has no size to hold against");
    MemoryAsk {
        command: "cargo build --release".into(),
        bytes,
        ceiling,
        available: mem.available,
        total: mem.total,
    }
}

/// Wait for a card and answer it with `option`, handing back the event it was posed as.
fn answer_a_card(rx: &Receiver<Inbound>, head: &mut HeadClient, option: &str) -> SessionEvent {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(inbound) => {
                let ServerFrame::Event(envelope) = inbound.frame() else {
                    continue;
                };
                if let SessionEvent::DecisionRequested { req_id, .. } = &envelope.event {
                    head.answer(req_id, option)
                        .expect("the head answers the card");
                    return envelope.event;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
        assert!(
            Instant::now() < deadline,
            "no memory card arrived within {PATIENCE:?}"
        );
    }
}

/// **One ask, one card, one answer.** The card the model's `memory` above the ceiling produces is
/// answered by the operator's head: `grant_memory` runs this call only, `grant_memory_session`
/// covers what follows it, and the once-one covers nothing after it.
#[test]
fn a_head_answers_the_memory_card_and_the_two_grants_mean_what_they_say() {
    let Some(_) = letibot_tokencore::apparatus::applies(
        "`/proc/meminfo`, where the box's free memory is read",
        cfg!(target_os = "linux"),
    ) else {
        return;
    };
    let mem = Meminfo::read().expect("the numbers this platform reads them from");
    let ask = an_uncapped_ask(&mem);

    let socket = socket_path("grant");
    let hub = Hub::new("s-memory-card");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let server = serve_registry(Registry::of(hub.clone()), &socket).expect("the socket binds");

    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let (mut head, _hello, reader) =
        HeadClient::attach(&socket, "s-memory-card", 0, "tui", "dead", caps)
            .expect("a head attach");
    let (tx, rx) = channel();
    std::thread::spawn(move || pump(reader, tx));

    // The head's half, on its own thread: the tool thread is the one that blocks on the answer.
    let (cards, card_rx) = channel();
    let head_thread = std::thread::spawn(move || {
        for option in ["grant_memory", "grant_memory_session"] {
            let event = answer_a_card(&rx, &mut head, option);
            let _ = cards.send(event);
        }
    });

    let mut gate = AdjudicatedGate::new(Box::new(
        HeadAdjudicator::new(hub.clone(), answers.clone()).with_budget(PATIENCE * 2),
    ));
    assert_eq!(
        gate.grant_memory(&ask),
        MemoryGrant::Granted,
        "the operator let this one command have it"
    );

    // The first card: what the operator read, and what they could choose.
    let Some(SessionEvent::DecisionRequested {
        kind,
        options,
        summary,
        target,
        because,
        ..
    }) = card_rx.recv_timeout(PATIENCE).ok()
    else {
        panic!("the first ask was not posed as a permission card");
    };
    assert_eq!(kind, "permission", "a memory ask is a permission");
    assert_eq!(
        target, "cargo build --release",
        "the card names the command it is about"
    );
    assert!(
        summary.contains("NO memory cap") && summary.contains("cargo build --release"),
        "{summary}"
    );
    assert!(
        because.contains("is free now"),
        "the card says how much is free, which is what the ceiling is a share of: {because}"
    );
    let ids: Vec<&str> = options.iter().map(|o| o.option_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["grant_memory", "grant_memory_session", "deny"],
        "the memory card's own three options"
    );

    // A second ask of the same size is a second card: a once-grant is once.
    assert_eq!(gate.grant_memory(&ask), MemoryGrant::Granted);
    let Some(SessionEvent::DecisionRequested { .. }) = card_rx.recv_timeout(PATIENCE).ok() else {
        panic!("the second ask must be a card: a once grant covers nothing after it");
    };

    // And the session grant covers the third: no card, so nothing to answer.
    assert_eq!(
        gate.grant_memory(&ask),
        MemoryGrant::Granted,
        "the session grant covers an ask no larger than what was granted"
    );
    head_thread.join().expect("the head's thread");
    assert!(
        hub.snapshot().open_decisions.is_empty(),
        "an answered card is off the head's screen"
    );
    drop(server);
}
