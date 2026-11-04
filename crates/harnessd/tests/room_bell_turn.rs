//! Rule 4 - a bell must be able to inject a turn, proved on a live session rather than
//! assumed from the wiring. `crates/room/tests/bell.rs` proves what a CLIENT hears on the
//! socket; what is asserted here is the hop after it

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Parts;
use letibot_harnessd::{Dialect, Harness};
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Request, Response, Room, Speaker};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker as Row, ToolCall, TranscriptItem};

mod support;

use support::canned;
use support::qwen::a_plain_answer_turn;

/// The sentence the room pushes, as the link composes it - matched rather than reproduced, so a
/// reworded `wake::sentence` fails in one place instead of two.
const POSTED: &str = "posted #2 under `ram`";

/// How long a correct build takes milliseconds - generous on purpose, because the assertion is
/// what happened rather than how fast.
const PATIENCE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-bell-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    dir
}

fn config(store: &std::path::Path, session: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session.to_string();
    // The link is the subject, asked for explicitly rather than inherited: a constructor that
    // stopped setting it would make this test measure nothing.
    cfg.room_link = true;
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

/// Everything the session's log holds as a user row - the rows a prompt is built from, and the
/// only place a model's own notice can be asserted without a live model.
fn user_rows(hub: &Hub) -> Vec<(Row, String)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. }
            | SessionEvent::TranscriptAppended {
                item: Some(item), ..
            } => match &**item {
                TranscriptItem::User { speaker, parts } => Some((
                    *speaker,
                    parts
                        .iter()
                        .filter_map(|p| match p {
                            letibot_transcript::UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The assistant's own answers - how a turn that RAN is told from one that was merely queued.
fn answers(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. }
            | SessionEvent::TranscriptAppended {
                item: Some(item), ..
            } => match &**item {
                TranscriptItem::Assistant { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Take every wake already queued, so the next one is the one the test made. Bounded by a
/// count rather than by the clock: the link rings for events this session caused, and a
/// wait for a quiet bell would measure the box's speed.
fn drain_the_bell(registry: &Registry) {
    for _ in 0..64 {
        match registry.next_work_until(Some(Instant::now())) {
            WorkOrIdle::Work(_) => continue,
            WorkOrIdle::Idle => return,
            WorkOrIdle::Closed => panic!("the registry closed under the test"),
        }
    }
}

/// Wait for this session to be rung - `Work::Woken`, which is the only thing that tells a
/// worker *something inside the daemon woke this session and no head asked for it*.
fn wait_for_a_wake(registry: &Registry, session: &str) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match registry.next_work_until(Some(deadline)) {
            WorkOrIdle::Work(Work::Woken(id)) if id == session => return,
            // Any other work is somebody else's: the bell is per session, so it is drained and
            // the wait goes on rather than failing on a neighbour's traffic.
            WorkOrIdle::Work(_) => continue,
            WorkOrIdle::Idle => panic!(
                "the room event never rang `{session}`'s bell — the socket reader pushed a \
                 sentence and nothing told the worker to serve it, which is the mailbox this rule \
                 exists to rule out"
            ),
            WorkOrIdle::Closed => panic!("the registry closed under the test"),
        }
    }
}

/// Wait until the session has `n` sentences waiting for its next turn - the assertion a test makes
/// instead of sleeping.
fn wait_for_sentences(session: &str, at_least: usize) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let waiting = letibot_harnessd::room::wake::waiting(session);
        if waiting >= at_least {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "the room's sentence never reached `{session}`'s queue: {waiting} waiting after \
                 {PATIENCE:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Rule 4, on a live session
// ---------------------------------------------------------------------------

/// A room event starts a turn in a session nobody typed into. A real `Harness` opened the
/// way a daemon opens one, with a canned model behind it; another session's `post` is the
/// bell. Asserted in the order the chain runs
#[test]
fn a_room_event_starts_a_turn_in_a_session_nobody_typed_into() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = scratch("turn");
    let store = dir.join("sessions.db");
    let socket = dir.join("room.sock");
    let session = "s-bell-turn";

    // The room first, served as the coordinator serves it, pointed at by the one door a
    // fixture has (`LETIBOT_ROOM_SOCKET`).
    let server = Server::serve(&socket, Room::new()).expect("a room to talk to");
    // SAFETY: this binary's first test sets the variable for the length of its own run, and the
    // other test in this file reads no socket at all. `room::socket_path` is read once, when the
    // harness opens below.
    unsafe { std::env::set_var(letibot_harnessd::room::SOCKET_ENV, &socket) };

    let mut cfg = config(&store, session);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    // Held for the whole test, not merely for its endpoint: `Canned` owns the server thread
    // and one dropped here would answer nothing and read as a model that never replied.
    let canned = canned::Canned::serve_each(
        vec![a_plain_answer_turn(
            &parts.vocab,
            "reading the room",
            "noted",
            40,
        )],
        4,
    );
    cfg.endpoint = canned.endpoint.clone();
    let registry = Registry::new();
    let hub = registry.new_hub(session.to_string());
    registry
        .adopt(
            hub.clone(),
            session.to_string(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: cfg.workspace.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    let mut harness = Harness::open_with_registry(
        &parts,
        cfg.clone(),
        hub.clone(),
        None,
        None,
        registry.clone(),
    )
    .expect("the session opens");
    // The link exists, the premise of everything below: without it the failure arrives three
    // assertions later as a timeout that reads like a slow box.
    assert!(
        harness.runtime_handle().registry.get("room").is_some(),
        "a top-level session with a coordinator on the box is seated the `room` tool: {:?}",
        harness.runtime_handle().registry.names()
    );

    // Another session's work, and this session becomes a member of the tag by claiming it -
    // the production door, through the tool the model would call.
    // In the harness session's own project room: queues are per room, and the session's folder
    // is its own project, so a post is claimable only by a session of that project.
    let project = letibot_room::project_key(&cfg.workspace.display().to_string());
    let elsewhere = Client::connect_named(
        &socket,
        Speaker::top_level("s-elsewhere"),
        letibot_room::room_name(&server.name(), &project),
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("the other session joins the room");
    let posted = elsewhere
        .request(&Request::Post {
            id: "p1".into(),
            tag: "ram".into(),
            body: "8 GB for the suite".into(),
        })
        .expect("a post");
    assert!(matches!(posted, Response::Posted { .. }), "{posted:?}");
    let claimed = harness.runtime_handle().invoke(
        "t1",
        &ToolCall {
            id: "call_claim".into(),
            name: "room".into(),
            arguments: serde_json::json!({"verb": "claim", "tag": "ram"}).to_string(),
        },
        &mut NullToolSink,
    );
    assert!(
        claimed.payload.contains("#1"),
        "the session holds #1, which is what makes it a member of `ram`: {}",
        claimed.payload
    );

    // The claim's own notice is drained, not asserted on for its timing. This session caused
    // it, so the link rings it for its own news - that half is `room_bell.rs`'s subject. What the
    // drain buys is the next assertion being about ONE wake: the one this test made.
    wait_for_sentences(session, 1);
    let own = letibot_harnessd::room::wake::take(session);
    assert!(
        own.iter().any(|l| l.contains("`s-bell-turn` claimed #1")),
        "a session is told about its own claim — the sentence the link composed: {own:?}"
    );
    drain_the_bell(&registry);

    // ---- the bell ---------------------------------------------------------
    // A second item under the same tag, posted by somebody else. This session is queued under
    // `ram` (it holds #1), so the router addresses it - and it asked for nothing.
    let second = elsewhere
        .request(&Request::Post {
            id: "p2".into(),
            tag: "ram".into(),
            body: "the other 8 GB".into(),
        })
        .expect("a second post");
    assert!(matches!(second, Response::Posted { .. }), "{second:?}");

    wait_for_sentences(session, 1);
    wait_for_a_wake(&registry, session);

    // ---- the turn ---------------------------------------------------------
    let ran = harness
        .wake()
        .expect("a wake with a room sentence in it runs a turn");
    assert!(
        ran.is_some(),
        "`Harness::wake` answers `None` only when nothing had settled; the room's sentence had"
    );
    let rows = user_rows(&hub);
    let heard = rows
        .iter()
        .find(|(_, said)| said.contains(POSTED))
        .unwrap_or_else(|| {
            panic!("the room's sentence reached the model as a user row: {rows:#?}")
        });
    assert_eq!(
        heard.0,
        Row::Agent,
        "and in the harness's own voice, so a head does not draw it as the operator's words"
    );
    assert!(
        !answers(&hub).is_empty(),
        "the turn RAN: a wake that opened a turn and got no answer is the failure this rule names"
    );
    // And nothing is left behind. A sentence taken by this wake must not be served again by
    // the next one, or every later turn would repeat the same bell.
    assert_eq!(
        letibot_harnessd::room::wake::waiting(session),
        0,
        "the bell was answered once"
    );

    drop(elsewhere);
    drop(harness);
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Rule 4, for the clients that cannot hear one
// ---------------------------------------------------------------------------

/// The two clients with no room connection say so in their manifest, and the table says so
/// in words. The head (`letibot-tui`) and the bridge (`letibot-bridge`) are clients of the
/// DAEMON, not of the room
#[test]
fn the_clients_that_cannot_hear_a_room_event_say_so_where_a_person_looks() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        // `crates/harnessd` → `crates` → the workspace. Two levels, because this test judges
        // manifests the harnessd crate does not own.
        .parent()
        .and_then(|p| p.parent())
        .expect("the crate is two levels under the workspace")
        .to_path_buf();
    for (crate_name, why) in [
        (
            "tui",
            "the head draws the room's pane from the daemon's feed and the room's news as the turn \
             it starts; it holds no room connection and therefore draws no card for an event",
        ),
        (
            "bridge",
            "a bridge head is woken by the daemon's turns, asks and activity, so a room event \
             reaches `Head::wait` only by starting a turn — never as an `Ask`, and never as exit 10",
        ),
    ] {
        let manifest = root.join("crates").join(crate_name).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", manifest.display()));
        // The bridge takes the protocol number from the room crate and nothing else, so a
        // dependency is allowed there while no other `letibot_room` item is named in its source.
        if crate_name == "bridge" && text.contains("letibot-room") {
            let mut used = String::new();
            for entry in std::fs::read_dir(root.join("crates/bridge/src")).expect("bridge src") {
                let src =
                    std::fs::read_to_string(entry.expect("an entry").path()).unwrap_or_default();
                used.push_str(&src);
            }
            let named = used.matches("letibot_room::").count();
            let allowed = used.matches("letibot_room::ROOM_PROTOCOL_VERSION").count()
                + used.matches("letibot_room::wire::upgrade_hint").count();
            assert_eq!(
                named, allowed,
                "`bridge` may name only the protocol number and the upgrade hint from the room crate"
            );
            continue;
        }
        assert!(
            !text.contains("letibot-room"),
            "`{crate_name}` has grown a room connection. {why}. If that is intended, the client is \
             a fifth row of `docs/room.md` § *The bell, per client* and this test is the sentence \
             to rewrite — the table and the code must not disagree about who can be woken."
        );
    }
    // And the doc is where a person looks, so the table is asserted to exist rather than
    // assumed: a paragraph deleted in a rewrite would leave the absence undiscoverable again.
    let doc = std::fs::read_to_string(root.join("docs").join("room.md"))
        .expect("docs/room.md is the room's own description");
    for wanted in [
        "The bell, per client",
        "letibot-tui",
        "letibot-bridge",
        "firecode",
    ] {
        assert!(
            doc.contains(wanted),
            "docs/room.md § *The bell, per client* must name `{wanted}`: the absence of a bell in a \
             client is only honest where a person can read it"
        );
    }
}
