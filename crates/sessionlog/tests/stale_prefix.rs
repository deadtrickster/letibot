//! **The stale-prefix sentence is said at ATTACH, and that is the whole point of it.**
//!
//! The comparison is made by the daemon's harness when it opens a session — the only
//! moment the two facts (the prefix a session was seated with, and the prefix this daemon
//! would compose now) are in one place — and the SENTENCE belongs here, on the attach
//! path, because of a property of the head: a warning a head finds in its snapshot is
//! filed [`letibot_sessionlog::view::Placed`]`::Before` — listed by `/notes`, counted by
//! `/status`, and **not drawn**. Every ordinary path opens the session before any head is
//! on it (the daemon opens its first session at startup; a `ResumeSession` is answered on
//! the worker before the head switches), so a warning published at open reaches a head
//! that will never draw it.
//!
//! So the harness leaves the sentence in the registry (`Registry::set_stale_prefix`) and
//! `server::seat_in` publishes it once the head is registered — the frame order on the
//! socket is `Hello`, then this event, and the head draws it at the end of the
//! conversation it is looking at.
//!
//! # Both halves, because a notice that fires on every attach is as useless as one that
//! never fires
//!
//! The negative case is asserted with a **marker** rather than a timeout: a warning
//! published after the attach is queued behind anything `seat_in` said, so a reader that
//! reaches the marker without having seen a `prefix_stale` has been told nothing — and
//! that is a fact, not a race against a clock.

use std::sync::Arc;

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-stale-{tag}-{}-{n}.sock",
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

/// The sentence the daemon's harness would have left behind.
const SAID: &str = "session s was seated 10-08 21:14 with a prompt this daemon would not \
                    compose now: the tool schemas have changed since. This session keeps \
                    the prefix it was created with, so the model cannot call a tool seated \
                    since. `/reseat` rebuilds the prompt from what is seated now, carrying \
                    this conversation across as it is; a new session gets the seated one \
                    from the start.";

#[test]
fn a_head_attaching_to_a_stale_session_is_told_while_it_watches() {
    let (registry, _hub, _server, path) = up("told");
    registry.set_stale_prefix("s", Some(SAID.to_string()));

    let (_client, hello, mut reader) =
        HeadClient::attach(&path, "s", 0, "tui", "operator", Caps::default()).expect("attach");
    assert!(
        matches!(hello, ServerFrame::Hello { .. }),
        "the handshake is the Hello, as it always was: {hello:?}"
    );

    // **The very next frame.** `seat_in` publishes after `attach` registers this head and
    // before the pump starts, so the sentence cannot arrive later, out of order, or as
    // part of the snapshot — which is the difference between a head that draws it and a
    // head that only counts it.
    let frame: ServerFrame = reader.read().expect("the sentence");
    let ServerFrame::Event(env) = frame else {
        panic!("expected the warning as the next frame, got {frame:?}");
    };
    let SessionEvent::Warning { code, detail, .. } = env.event else {
        panic!("expected a warning, got {:?}", env.event);
    };
    assert_eq!(
        code, "prefix_stale",
        "the code the head classifies and draws"
    );
    assert!(
        detail.contains("`/reseat`"),
        "the remedy travels in the sentence: {detail}"
    );
    assert!(
        detail.contains("was seated"),
        "and when the session was seated: {detail}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_head_attaching_to_a_session_that_speaks_the_seated_prompt_is_told_nothing() {
    let (registry, hub, _server, path) = up("silent");
    // The ordinary case: the harness found the two prefixes equal, or never ran at all.
    registry.set_stale_prefix("s", None);

    let (_client, _hello, mut reader) =
        HeadClient::attach(&path, "s", 0, "tui", "operator", Caps::default()).expect("attach");

    // Published AFTER the attach, so it is queued behind anything `seat_in` said.
    hub.publish(letibot_sessionlog::testing::warn("marker"));
    let mut seen = 0usize;
    loop {
        let frame: ServerFrame = reader.read().expect("the marker");
        let ServerFrame::Event(env) = frame else {
            continue;
        };
        if let SessionEvent::Warning { ref code, .. } = env.event {
            if code == "prefix_stale" {
                panic!("an unchanged prefix was reported stale: {:?}", env.event);
            }
            if code == "test" && env.event == letibot_sessionlog::testing::warn("marker") {
                break;
            }
        }
        seen += 1;
        assert!(seen < 1_000, "the marker never arrived");
    }

    let _ = std::fs::remove_file(&path);
}

/// **A session nobody has opened says nothing**, which is not the same as *unchanged*.
///
/// There is no harness, so nothing compared anything, and the registry has no sentence to
/// hand over. The attach is still served — a head can attach to a session the daemon has
/// not opened — and the silence here is the absence of a comparison rather than a verdict.
#[test]
fn a_session_no_harness_has_opened_says_nothing() {
    let (registry, hub, _server, path) = up("unopened");
    assert_eq!(
        registry.stale_prefix("s"),
        None,
        "nothing has opened this session, so nothing can say"
    );

    let (_client, _hello, mut reader) =
        HeadClient::attach(&path, "s", 0, "tui", "operator", Caps::default()).expect("attach");
    hub.publish(letibot_sessionlog::testing::warn("marker"));
    let frame: ServerFrame = reader.read().expect("the marker");
    let ServerFrame::Event(env) = frame else {
        panic!("expected the marker, got {frame:?}");
    };
    assert_eq!(
        env.event,
        letibot_sessionlog::testing::warn("marker"),
        "the first thing this head hears is the marker, not a notice about a \
         comparison nobody made"
    );

    let _ = std::fs::remove_file(&path);
}
