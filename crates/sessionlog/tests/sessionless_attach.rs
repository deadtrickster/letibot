//! R178: a daemon whose last session closed keeps serving. The bare attach a head makes with no
//! session in mind lands on nothing, so it is served as a control connection that holds no session
//! and may only make one - and the refusal for an id this daemon does not hold names what it DOES
//! hold live, never the store's index, which since the split is every project's.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{ClientError, HeadClient};
use letibot_sessionlog::protocol::{
    Caps, NOTE_SESSION_CLOSED, REJECT_UNKNOWN_SESSION, ServerFrame,
};
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::wire::FrameReader;

fn socket_in(name: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("sessionless-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    letibot_sessionlog::testing::socket_path(&dir, "harnessd")
}

/// Read frames until one satisfies `want`, or the deadline passes: the pump may deliver a
/// snapshot's events first, so a bare `read` next is not enough.
fn until(reader: &mut FrameReader<UnixStream>, want: impl Fn(&ServerFrame) -> bool) -> ServerFrame {
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

fn attach(socket: &PathBuf, session: &str) -> (HeadClient, ServerFrame, FrameReader<UnixStream>) {
    HeadClient::attach(socket, session, 0, "tui", "operator", Caps::default())
        .expect("the attach is answered")
}

/// The sentence that refused an attach: a `Bye` is the only frame that answers one.
fn refused(socket: &PathBuf, session: &str) -> String {
    match HeadClient::attach(socket, session, 0, "tui", "operator", Caps::default()) {
        Err(ClientError::Refused(reason)) => reason,
        Err(other) => panic!("attaching to {session:?}: {other}"),
        Ok(_) => panic!("the daemon served an attach it had to refuse: {session:?}"),
    }
}

#[test]
fn a_daemon_whose_only_session_closed_still_takes_a_bare_attach_and_makes_one() {
    let registry = Registry::new();
    registry
        .create("s-only", "the one that closes", SessionWiring::default())
        .expect("a session to close");
    let socket = socket_in("only");
    let _server = serve_registry(registry.clone(), &socket).expect("bind");

    // The operator closes the only session. The daemon stays up holding none - the state that
    // made every later `leticode_start` fail (R178).
    {
        let (mut head, hello, mut reader) = attach(&socket, "s-only");
        assert!(
            matches!(&hello, ServerFrame::Hello { session_id, .. } if session_id == "s-only"),
            "the first attach lands on the session: {hello:?}"
        );
        let id = head
            .close_session(0, "operator", "done with this one")
            .expect("send the close");
        match until(
            &mut reader,
            |f| matches!(f, ServerFrame::Accepted { client_request_id, .. } if *client_request_id == id),
        ) {
            ServerFrame::Accepted { note, .. } => assert_eq!(note, NOTE_SESSION_CLOSED),
            other => panic!("{other:?}"),
        }
        let _ = head.detach();
    }
    assert!(
        registry.is_empty(),
        "the close leaves the daemon holding nothing live"
    );

    // The bare attach: no session to land on, and served anyway.
    let (mut control, hello, mut reader) = attach(&socket, "");
    match &hello {
        ServerFrame::Hello {
            session_id,
            head_id,
            sessions,
            ..
        } => {
            assert_eq!(
                session_id, "",
                "no session was invented for the attach: {hello:?}"
            );
            assert_eq!(head_id, "", "a head id names a seat inside a session");
            assert!(sessions.is_empty(), "it holds none: {sessions:?}");
        }
        other => panic!("expected a control Hello, got {other:?}"),
    }

    // A frame that acts on a session is refused by name rather than done.
    control
        .list_sessions()
        .expect("the list is one of the frames a control connection may send");
    match until(&mut reader, |f| matches!(f, ServerFrame::Sessions { .. })) {
        ServerFrame::Sessions { current, .. } => assert_eq!(current, ""),
        other => panic!("{other:?}"),
    }
    let asked = control.prompt(0, "are you there").expect("the frame goes");
    match until(
        &mut reader,
        |f| matches!(f, ServerFrame::Rejected { client_request_id, .. } if *client_request_id == asked),
    ) {
        ServerFrame::Rejected { reason, .. } => assert!(
            reason.contains("holds no session"),
            "the refusal says why: {reason}"
        ),
        other => panic!("{other:?}"),
    }

    // And the door that matters: the session this daemon could not otherwise be given.
    control
        .new_session("the next one", "/tmp/the-next-one")
        .expect("the new-session frame goes");
    let made = match until(&mut reader, |f| {
        matches!(
            f,
            ServerFrame::Sessions {
                created: Some(_),
                ..
            }
        )
    }) {
        ServerFrame::Sessions { created, .. } => created.expect("the id it made"),
        other => panic!("{other:?}"),
    };
    assert!(
        registry.get(&made).is_some(),
        "the daemon holds the session it minted"
    );
    assert_eq!(
        registry.default_id(),
        made,
        "and a bare attach lands on it from now on"
    );
    assert_eq!(
        registry.wiring(&made).workspace,
        "/tmp/the-next-one",
        "the head's tree is the one fact the frame brought"
    );

    // The session a head will hold: attachable by name, and the `Hello` names it.
    let (_head, hello, _reader) = attach(&socket, &made);
    assert!(
        matches!(&hello, ServerFrame::Hello { session_id, .. } if *session_id == made),
        "the new session opens to a head: {hello:?}"
    );
}

/// What a store answers, without one: since the split its index is every project's sessions.
struct OtherProjects(Vec<StoredBrief>);

impl SessionSource for OtherProjects {
    fn list(&self) -> Vec<StoredBrief> {
        self.0.clone()
    }
}

fn stored(id: &str) -> StoredBrief {
    StoredBrief {
        session_id: id.into(),
        title: String::new(),
        items: 1,
        last_activity_ms: 1_788_990_321_957,
        wiring: SessionWiring::default(),
        parent_session_id: None,
        context_tokens: None,
        context_cached: None,
        stored_end: None,
        nags_paused: None,
        waits_on_operator: 0,
        slug: String::new(),
        origin: Default::default(),
    }
}

/// An unknown id is answered with the sessions THIS daemon holds, ten of them and a count: the
/// store's index is another project's business, and it was thousands of ids long.
#[test]
fn the_refusal_names_only_the_sessions_this_daemon_holds_live() {
    let registry = Registry::new();
    let elsewhere: Vec<StoredBrief> = (1..=30).map(|n| stored(&format!("s-other-{n}"))).collect();
    registry.set_source(Arc::new(OtherProjects(elsewhere)));
    for n in 1..=12 {
        registry
            .create(format!("s-live-{n}"), "", SessionWiring::default())
            .expect("a live session");
    }
    let socket = socket_in("refusal");
    let _server = serve_registry(registry.clone(), &socket).expect("bind");

    let reason = refused(&socket, "s-nowhere");
    assert!(reason.contains(REJECT_UNKNOWN_SESSION), "{reason}");
    assert!(reason.contains("\"s-nowhere\""), "{reason}");
    for n in 1..=10 {
        assert!(
            reason.contains(&format!("s-live-{n}")),
            "the first ten live ids are named: {reason}"
        );
    }
    assert!(
        !reason.contains("s-live-11") && !reason.contains("s-live-12"),
        "the list is capped: {reason}"
    );
    assert!(
        reason.contains("and 2 more"),
        "the cap is counted: {reason}"
    );
    assert!(
        !reason.contains("s-other-"),
        "the store's index is never named: {reason}"
    );
}

/// A daemon holding nothing live says that instead of listing the store's rows - the other half
/// of the same sentence, and the one a `leticode_start` reads when its daemon is up and empty.
#[test]
fn a_daemon_holding_nothing_live_names_no_session_at_all() {
    let empty = Registry::new();
    empty.set_source(Arc::new(OtherProjects(vec![stored("s-other-1")])));
    let socket = socket_in("refusal-empty");
    let _server = serve_registry(empty, &socket).expect("bind");

    let reason = refused(&socket, "s-other-1");
    assert!(
        reason.contains("holds none live"),
        "no session is named because none is held: {reason}"
    );
    assert!(!reason.contains("s-other-2"), "{reason}");
}
