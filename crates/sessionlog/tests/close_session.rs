//! A close is one session gone while the daemon keeps serving the rest - the test holds two and
//! asserts the other survives. [`Work::Close`] is the one ring item not filtered for *the registry
//! no longer holds it*: dropping it would hold the harness for the daemon's life.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, NOTE_SESSION_CLOSED, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::wire::FrameReader;

fn socket_in(name: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("close-session-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    letibot_sessionlog::testing::socket_path(&dir, "harnessd")
}

/// Read frames until one satisfies `want`, or the deadline passes. The pump may deliver a
/// snapshot's events first, so a bare `read` next is not enough.
fn until(
    reader: &mut FrameReader<std::os::unix::net::UnixStream>,
    want: impl Fn(&ServerFrame) -> bool,
) -> ServerFrame {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "the daemon never answered what this test waited for"
        );
        let frame: ServerFrame = reader.read().expect("a frame");
        if want(&frame) {
            return frame;
        }
    }
}

#[test]
fn closing_one_session_leaves_the_others_and_rings_the_worker_to_let_go() {
    let registry = Registry::new();
    registry
        .create("s-closed", "the one that moves", SessionWiring::default())
        .expect("a session to close");
    registry
        .create("s-stays", "the one that does not", SessionWiring::default())
        .expect("a session that stays");

    let socket = socket_in("server");
    let server = serve_registry(registry.clone(), &socket).expect("bind");

    let (mut head, hello, mut reader) =
        HeadClient::attach(&socket, "s-closed", 0, "tui", "operator", Caps::default())
            .expect("attach");
    match &hello {
        ServerFrame::Hello {
            protocol_version, ..
        } => {
            assert_eq!(*protocol_version, PROTOCOL_VERSION)
        }
        other => panic!("expected a Hello, got {other:?}"),
    }

    let id = head
        .close_session(0, "operator", "the operator is closing it")
        .expect("send the close");
    let answer = until(
        &mut reader,
        |f| matches!(f, ServerFrame::Accepted { client_request_id, .. } if *client_request_id == id),
    );
    match answer {
        ServerFrame::Accepted { note, .. } => assert_eq!(
            note, NOTE_SESSION_CLOSED,
            "the head has to be able to tell `this session is closed` from `the daemon is \
             stopping`: one means find the conversation elsewhere, the other means wait for \
             the socket to go"
        ),
        other => panic!("{other:?}"),
    }

    assert!(
        registry.get("s-closed").is_none(),
        "the closed session must be out of the registry: that is what makes the copy taken \
         after this the last word on the conversation"
    );
    assert!(
        registry.get("s-stays").is_some(),
        "a close is ONE session — the daemon holds the operator's other conversations and a \
         frame that took them with it would be `Stop` wearing a different name"
    );

    // And the worker is told to drop the harness, which is the half only it can do. Other work
    // may come first - the session that stays can be opened or woken before the close reaches the
    // worker - so what matters is that the close arrives before the deadline.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match registry.next_work_until(Some(deadline)) {
            WorkOrIdle::Work(Work::Close(id)) => {
                assert_eq!(id, "s-closed");
                break;
            }
            WorkOrIdle::Work(Work::Open(_))
            | WorkOrIdle::Work(Work::Command(..))
            | WorkOrIdle::Work(Work::Woken(_)) => continue,
            WorkOrIdle::Idle => {
                panic!("the worker was not rung to let go, so the harness stays held")
            }
            WorkOrIdle::Closed => {
                panic!("the registry closed, which a close of ONE session must not")
            }
        }
    }

    drop(reader);
    drop(head);
    server.shutdown();
}
