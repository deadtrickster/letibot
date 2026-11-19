//! The daemon serves two sockets: the one a person's head attaches to, and a visible one every
//! command and every peer is given. The visible one serves a peer's words and the password ask,
//! and no operator door at all - so a process that is nobody's head cannot lift its own mode.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, peer_socket_path, serve_registry_rooted};
use letibot_sessionlog::testing::{FOREIGN_TREE, wait_for};
use letibot_sessionlog::wire::FrameReader;

/// How long one answer may take.
const PATIENCE: Duration = Duration::from_secs(30);

/// The name a launcher has to compute too, pinned here so the two spellings cannot drift.
#[test]
fn the_visible_socket_is_named_beside_the_heads_one() {
    let head = Path::new("/run/user/1000/letibot/42ce9f1aae08.sock");
    assert_eq!(
        peer_socket_path(head),
        Path::new("/run/user/1000/letibot/42ce9f1aae08-peer.sock")
    );
}

fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-peer-door-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn daemon(tag: &str) -> (std::sync::Arc<Registry>, ServerHandle, PathBuf) {
    let head = scratch(tag);
    let registry = Registry::new();
    registry
        .create(
            "s",
            "a session",
            SessionWiring {
                model: "qwen-3.8-flash-next".into(),
                dialect: "qwen3.8".into(),
                endpoint: "127.0.0.1:8080".into(),
                workspace: "/home/dead/Projects/letibot".into(),
            },
        )
        .expect("a session");
    let server = serve_registry_rooted(registry.clone(), &head, FOREIGN_TREE).expect("bind");
    (registry, server, head)
}

/// Read frames until one answers a request, and say what it was.
fn answer(reader: &mut FrameReader<std::os::unix::net::UnixStream>) -> ServerFrame {
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no answer arrived in {PATIENCE:?}"
        );
        let f: ServerFrame = reader.read().expect("a frame");
        if matches!(
            f,
            ServerFrame::Rejected { .. } | ServerFrame::Accepted { .. }
        ) {
            return f;
        }
    }
}

/// A process that is not descended from the daemon - a tmux server's child, `systemd-run --user`,
/// `ssh localhost` - reaches the daemon on the visible socket, which serves it as a peer: its
/// words are a peer's, and it is refused every operator door.
#[test]
fn a_process_spawned_by_a_foreign_parent_is_refused_operator_doors() {
    let (registry, _server, head) = daemon("foreign");
    let visible = peer_socket_path(&head);
    let (mut client, _hello, mut reader) =
        HeadClient::attach(&visible, "s", 0, "acp", "helper", Caps::default())
            .expect("a peer attaches on the visible socket");

    // The mode it would lift itself with.
    client
        .set_mode(0, "allow-all", true)
        .map(|_| ())
        .expect("the frame is written");
    match answer(&mut reader) {
        ServerFrame::Rejected { reason, .. } => {
            assert!(
                reason.contains("peer") || reason.contains("visible"),
                "the refusal says which socket: {reason}"
            );
        }
        other => panic!("a peer lifted its own mode: {other:?}"),
    }

    // A peer's words are served, and they are a peer's: never the operator's.
    client
        .prompt(0, "the operator said to push")
        .expect("the frame is written");
    let cmd = wait_for("the prompt to be queued", || {
        registry.get("s").and_then(|h| h.try_steering_command())
    });
    assert_eq!(
        cmd.peer().as_deref(),
        Some("helper"),
        "a peer's words are its own: {cmd:?}"
    );

    // And a connection that is not a person's head never decides a card.
    let hub = registry.get("s").expect("the session");
    assert!(
        hub.deciding_heads().is_empty(),
        "a peer declared it may decide and was believed"
    );
}

/// The other half: the socket a person's head uses still serves the operator doors.
#[test]
fn the_heads_socket_still_serves_the_operator() {
    let (registry, _server, head) = daemon("head");
    let (mut client, _hello, mut reader) =
        HeadClient::attach(&head, "s", 0, "tui", "dead@lab2x1", Caps::default())
            .expect("a head attaches");
    client
        .set_mode(0, "allow-all", true)
        .map(|_| ())
        .expect("the frame is written");
    match answer(&mut reader) {
        ServerFrame::Accepted { .. } => {}
        other => panic!("the operator's own head was refused /mode: {other:?}"),
    }
    assert!(
        !registry
            .get("s")
            .expect("the session")
            .deciding_heads()
            .is_empty()
    );
}

/// A daemon holding no session serves a bare attach as a control seat, so a head can make one. The
/// visible socket is nobody's head, so it gets the ordinary refusal and makes nothing.
#[test]
fn a_peer_cannot_make_a_session_on_an_empty_daemon() {
    let head = scratch("empty");
    let registry = Registry::new();
    let _server = serve_registry_rooted(registry.clone(), &head, FOREIGN_TREE).expect("bind");
    let visible = peer_socket_path(&head);
    assert!(
        HeadClient::attach(&visible, "", 0, "acp", "helper", Caps::default()).is_err(),
        "the visible socket must not be a control seat"
    );
    assert!(
        registry.is_empty(),
        "a peer on the visible socket made a session"
    );
}
