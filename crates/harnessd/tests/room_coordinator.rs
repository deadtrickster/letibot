//! **The arbiter can rule** — a `--room` coordinator's own session, opened for real, with the
//! `room` tool on its seat and a raised item reaching a ruling through it.
//!
//! # What this file exists to have caught
//!
//! The coordinator was refused at `9a0e7426` for one defect that made the whole feature silently
//! useless. `coordinator::configure` seats `Seat::Orchestrator`; `harness.rs`'s `base_role_for_seat`
//! resolves that to `roles::m1_orchestrator()`; and `Registry::resolve_role` keeps **only** the
//! tools a role names. The `room` tool was registered by `Harness::open` and named by
//! `roles::orchestrator()` — §8.4's table, which **nothing seats** — so the coordinator's own
//! registry had no `room` in it. The tie-breaker thread was then woken, handed a prompt saying
//! *"answer with the `room` tool: verb `rule`"*, and had no such tool: a raised item stayed frozen
//! for ever, which is the exact outcome `configure`'s no-model refusal says it exists to prevent.
//!
//! `coordinator.rs`'s own test exercised `Coordinator::start` with **raw clients**, so it proved
//! the lease, the socket, the server and the ruling — and never asked what tools the session that
//! does the ruling actually has. This file asks that, and then does the ruling through the tool.
//!
//! # What it needs
//!
//! Nothing on the box: the byte vocabulary and no provider (`vocab_gguf = None`, no
//! `cfg.provider`), so no GGUF, no model server and no key. `configure` fills the default model
//! only when both `provider` and `model` are empty, and the test names `local` — which is what a
//! coordinator whose arbiter runs on this box's own llama.cpp says. No turn is ever run: the
//! question is raised by two raw clients and answered by one tool call, and the arbiter's model is
//! never asked anything.
//!
//! # The socket
//!
//! `LETIBOT_ROOM_SOCKET` is process-wide, so this file holds **one** test: a second one in the
//! same binary would race it for the variable. The socket, the lease and the coordinator's
//! workspace all live in a directory this test owns, so nothing touches `~/.local/share/letibot`
//! and no coordinator is started against the operator's data directory.

use std::sync::Arc;

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::harness::Parts;
use letibot_harnessd::room::coordinator::{Coordinator, configure};
use letibot_harnessd::{Harness, room as daemon_room};
use letibot_room::client::Client;
use letibot_room::{Request, Response, Speaker};
use letibot_sessionlog::hub::Hub;
use letibot_tools::NullToolSink;
use letibot_transcript::{ToolCall, ToolOutcome};

/// A directory of this test's own, under the session scratch rather than a shared `/tmp`.
fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-room-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    dir
}

/// **The coordinator's own session, opened the way `cli::run` opens it**, and then a question
/// raised by two raw clients and ruled on through the session's `room` tool.
#[test]
fn a_coordinators_own_session_can_rule_through_its_room_tool() {
    let dir = scratch("coordinator-session");
    let socket = dir.join("room.sock");
    // SAFETY: this binary holds one test, so nothing else in the process reads this variable
    // while it is set. It is the door `daemon_room::socket_path` reads, and it is what keeps the
    // whole test inside `dir`.
    unsafe { std::env::set_var(daemon_room::SOCKET_ENV, &socket) };

    // **The config a `--room` daemon gets**, through the function that makes it one. `model` is
    // named so `configure` neither fills the deepseek default nor asks whether a key for it
    // resolves — this box is not being measured for its provider keys, and no turn is ever run.
    let workspace = dir.join("start");
    std::fs::create_dir_all(&workspace).expect("a starting workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.vocab_gguf = None;
    cfg.model = "local".to_string();
    configure(&mut cfg).expect("a coordinator's configuration");

    // The two facts the seat decision turns on, asserted where they are set rather than assumed.
    assert_eq!(
        cfg.seat,
        Seat::Orchestrator,
        "the coordinator's session opens on the seat that needs no adjudicator"
    );
    assert!(
        cfg.room_link,
        "and that session must be able to join the room: it is the arbiter"
    );
    let arbiter = cfg.session_id.clone();
    assert_eq!(arbiter, daemon_room::session_id());

    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    let hub = Hub::new(&arbiter);
    let rung = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |_: &str| {
            rung.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    };
    // **The coordinator first**, exactly as `cli::run` does it: the socket has to exist when
    // `Harness::open` builds the session's room link, and the session id has to be the arbiter's
    // before an event can be routed to it.
    let coord = Coordinator::start(&arbiter, Arc::clone(&bell)).expect("a coordinator");

    let h = Harness::open(&parts, cfg, hub).expect("the coordinator's session opens");

    // ---------------------------------------------------------------------------------------
    // 1. The registry the seat resolves has `room` in it — the assertion that was missing.
    // ---------------------------------------------------------------------------------------
    let seated = h.wiring().seated.clone();
    assert!(
        seated.iter().any(|t| t == "room"),
        "the arbiter's own seat must name `room`, or `resolve_role` drops it and the tie-breaker \
         is told to call a tool it does not have. Seated: {seated:?}"
    );
    let runtime = h.runtime_handle();
    assert!(
        runtime.registry.get("room").is_some(),
        "and the registry the session runs calls through must hold it: {:?}",
        runtime.registry.names()
    );

    // ---------------------------------------------------------------------------------------
    // 2. A question reaches the arbiter's session, and `rule` answers it through the TOOL.
    // ---------------------------------------------------------------------------------------
    let wake = Arc::new(|_: &letibot_room::Event| {});
    let a = Client::connect(&socket, Speaker::top_level("s-a"), wake)
        .expect("a session joins the room");
    let posted = a
        .request(&Request::Post {
            id: "p1".into(),
            tag: "ram".into(),
            body: "8 GB for the suite".into(),
        })
        .expect("a post");
    assert!(matches!(posted, Response::Posted { .. }), "{posted:?}");
    // Three abandonments raise the item — `ABANDON_THRESHOLD` — which is the only thing that
    // asks the arbiter anything.
    for k in 1..=3 {
        let _ = a.request(&Request::Claim {
            id: format!("c{k}"),
            tag: "ram".into(),
        });
        let _ = a.request(&Request::Abandon {
            id: format!("a{k}"),
            item: 1,
            why: "the suite blew up".into(),
        });
    }
    let open = coord.server().open_questions();
    assert_eq!(open.len(), 1, "three abandonments raise one question");
    let question = open[0].id;

    // **The arbiter is WOKEN before it is asked to rule.** Two things put a line in front of it:
    // the link's reader thread, which turns the `Raised` event into a sentence, and the
    // tie-breaker's own thread, which pushes the prompt naming the verb and rings. The queue is
    // process-wide and `take` empties it, so this drains it until the PROMPT is in hand rather
    // than assuming which of the two arrived first. A question nobody is woken for is a question
    // nobody answers, and that is the other half of *the arbiter can rule*.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut prompted: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline {
        prompted.extend(letibot_harnessd::room::wake::take(&arbiter));
        if prompted.iter().any(|line| line.contains("verb `rule`")) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        rung.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the tie-breaker must ring the arbiter's bell for a question it has not prompted"
    );
    assert!(
        prompted.iter().any(|line| line.contains("verb `rule`")),
        "the arbiter must be woken with the prompt that names the verb it has to call — this is \
         the sentence the whole defect was about: {prompted:?}"
    );

    // **The call the tie-breaker's prompt tells the model to make**, through the runtime the
    // session actually invokes through — not `SessionRoom::call` directly, because the whole
    // defect was a tool that was not there while the link underneath it was.
    let call = ToolCall {
        id: "call_rule".into(),
        name: "room".into(),
        arguments: serde_json::json!({
            "verb": "rule",
            "question": question,
            "decision": "drop",
            "why": "three claimants died on it, and two more are queued behind them",
        })
        .to_string(),
    };
    let mut sink = NullToolSink;
    let ruled = runtime.invoke("t1", &call, &mut sink);
    assert_eq!(ruled.outcome, ToolOutcome::Ok, "{ruled:?}");
    let said = ruled.render();
    assert!(
        said.contains(&format!("question #{question} is answered")),
        "the model must read that the question is answered: {said}"
    );
    assert!(
        coord.server().open_questions().is_empty(),
        "and the room must agree: nothing is left frozen"
    );

    // The item is out of the queue for good, which is what `drop` means — a ruling that only
    // emptied the question list would leave the item claimable and the queue still stuck.
    let queue = a
        .request(&Request::Queue { tag: "ram".into() })
        .expect("the queue reads");
    match queue {
        Response::Queue { rows, .. } => {
            assert!(
                rows.iter().all(|r| r.id != 1),
                "a dropped item leaves the queue: {rows:?}"
            );
        }
        other => panic!("a queue read answers rows: {other:?}"),
    }

    // **The bell rang** — the tie-breaker's own thread prompts the arbiter by pushing a sentence
    // and ringing; that is asserted above, before the ruling, because a question ruled within the
    // thread's two-second period is a question it never prompts.

    drop(h);
    drop(coord);
    // SAFETY: as above — this binary holds one test.
    unsafe { std::env::remove_var(daemon_room::SOCKET_ENV) };
    let _ = std::fs::remove_dir_all(&dir);
}
