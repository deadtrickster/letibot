//! **A daemon whose binary is older than the store it serves says so to EVERY head, on every
//! attach.**
//!
//! The operator's ruling of 2026-10-26, after a schema bump 32 → 33 (one nullable column) refused
//! every daemon built before it: *an older binary should keep working when the newer schema only
//! ADDED things, and must say so in red on the head's usual status line*.
//!
//! Keeping working is `letibot_tokencore`'s `store::floor` — the store records the lowest schema a
//! reader may know, and an additive migration leaves that floor where it was. **Saying so is this
//! file**: the daemon settles the fact once (`Registry::set_store_older`, from
//! `harnessd::store_floor`), and `server::seat_in` publishes it as `store_newer` after every
//! attach, because the same reason `prefix_stale` is said there applies twice over — a warning a
//! head finds in a snapshot is `Placed::Before` (counted, listed, **not drawn**), and this one is
//! true for the whole life of the process, so a head that arrives tomorrow is owed it as much as
//! the one that watched the daemon start.
//!
//! **The negative case is asserted with a marker rather than a timeout**, the way
//! `stale_prefix.rs` does it: a warning published after the attach is queued behind anything
//! `seat_in` said, so a reader that reaches the marker without having seen a `store_newer` has
//! been told nothing — a fact, not a race against a clock.

use std::sync::Arc;

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

/// What `harnessd::store_floor` composes: the store's own sentence, plus which binary is asking.
const SAID: &str = "this daemon (schema 33) is older than its store (34) — restart it: letibot \
                    --stop · running /home/dead/bin/letibot-harnessd, built 10-26 22:39";

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-store-newer-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// A registry holding one session, and the server accepting on it.
fn up(tag: &str) -> (Arc<Registry>, Arc<Hub>, ServerHandle, std::path::PathBuf) {
    let registry = Registry::new();
    let hub = registry
        .create("s", "a title", SessionWiring::default())
        .expect("the session must be created");
    let path = socket_path(tag);
    let server = serve_registry(registry.clone(), &path).expect("bind");
    (registry, hub, server, path)
}

/// **The sentence, as the first frame after the `Hello`** — `seat_in` publishes after `attach`
/// registers the head and before the pump starts, so it cannot arrive later or out of order.
fn told(path: &std::path::Path) -> String {
    let (_client, hello, mut reader) =
        HeadClient::attach(path, "s", 0, "tui", "operator", Caps::default()).expect("attach");
    assert!(
        matches!(hello, ServerFrame::Hello { .. }),
        "the handshake is the `Hello`, as it always was: {hello:?}"
    );
    let frame: ServerFrame = reader.read().expect("the sentence");
    let ServerFrame::Event(env) = frame else {
        panic!("expected the warning as the next frame, got {frame:?}");
    };
    let SessionEvent::Warning { code, detail, .. } = env.event else {
        panic!("expected a warning, got {:?}", env.event);
    };
    assert_eq!(code, "store_newer", "the code the head draws in red");
    detail
}

/// **Both heads are told, and the second one is told on its own attach.**
///
/// This is the claim that decides the shape of the whole feature: the fact is about the PROCESS,
/// it does not expire until a restart, and a head that arrives an hour later was not there for a
/// publish that happened once.
#[test]
fn every_head_that_attaches_is_told_the_store_is_newer() {
    let (registry, _hub, _server, path) = up("told");
    registry.set_store_older(Some(SAID.to_string()));

    for which in ["the first head", "a second head, later"] {
        let detail = told(&path);
        assert!(
            detail.contains("older than its store"),
            "{which} was told {detail}"
        );
        assert!(
            detail.contains("restart it: letibot --stop"),
            "the remedy travels in the sentence, or the reader is told nothing they can act on: \
             {detail}"
        );
        assert!(
            detail.contains("/home/dead/bin/letibot-harnessd, built "),
            "and WHICH binary it is, which is the operator's next question on a box with \
             several: {detail}"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// **A daemon that opened its own store says nothing**, which is every ordinary attach in this
/// tree — and the first thing a head hears is its own marker.
#[test]
fn a_daemon_that_can_read_its_store_says_nothing_about_it() {
    let (registry, hub, _server, path) = up("silent");
    registry.set_store_older(None);

    let (_client, _hello, mut reader) =
        HeadClient::attach(&path, "s", 0, "tui", "operator", Caps::default()).expect("attach");
    // Published AFTER the attach, so it is queued behind anything `seat_in` said.
    hub.publish(letibot_sessionlog::testing::warn("marker"));
    let frame: ServerFrame = reader.read().expect("the marker");
    let ServerFrame::Event(env) = frame else {
        panic!("expected the marker, got {frame:?}");
    };
    assert_eq!(
        env.event,
        letibot_sessionlog::testing::warn("marker"),
        "a daemon whose store is its own has nothing to say about it"
    );

    let _ = std::fs::remove_file(&path);
}
