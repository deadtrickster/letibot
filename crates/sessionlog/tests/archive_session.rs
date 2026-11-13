//! An archive is a session put out of sight and KEPT: the store's row carries the reason, a LIVE
//! session is closed as part of the act, and the daemon's other conversations are untouched.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::wire::FrameReader;

fn socket_in(name: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("archive-session-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    letibot_sessionlog::testing::socket_path(&dir, "harnessd")
}

/// Read frames until one satisfies `want`, or the deadline passes: the pump may deliver a
/// snapshot's events first.
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

/// What a store would answer, without one: every archive write, in order. `list` is empty because
/// this fixture's roster is the live sessions and nothing else.
#[derive(Default)]
struct Store(Mutex<Vec<(String, bool, String)>>);

impl SessionSource for Store {
    fn list(&self) -> Vec<StoredBrief> {
        Vec::new()
    }
    fn set_archived(&self, session_id: &str, archived: bool, reason: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((session_id.to_string(), archived, reason.to_string()));
        Ok(())
    }
}

impl Store {
    fn writes(&self) -> Vec<(String, bool, String)> {
        self.0.lock().unwrap().clone()
    }
}

/// A store with one session put out of sight in it: what a daemon that restarts onto it reads. The
/// live list is empty and the archived one is not, which is the state an archive leaves behind.
#[derive(Default)]
struct KeptStore;

impl SessionSource for KeptStore {
    fn list(&self) -> Vec<StoredBrief> {
        Vec::new()
    }
    fn list_archived(&self) -> Vec<StoredBrief> {
        vec![StoredBrief {
            session_id: "s-away".into(),
            title: "the one put away".into(),
            items: 3,
            last_activity_ms: 1_700_000_000_000,
            wiring: SessionWiring::default(),
            parent_session_id: None,
            context_tokens: None,
            context_cached: None,
            stored_end: None,
            nags_paused: None,
            waits_on_operator: 0,
            slug: String::new(),
            origin: Default::default(),
            role: String::new(),
        }]
    }
}

/// The frame a head sends, end to end: the answer is the new `Sessions` list, the archived session
/// leaves the registry (that IS the close), and the session the asking head is in survives.
#[test]
fn archiving_a_live_session_closes_it_and_writes_the_reason() {
    let registry = Registry::new();
    registry
        .create("s-away", "the one put away", SessionWiring::default())
        .expect("a session to archive");
    registry
        .create("s-here", "the one still open", SessionWiring::default())
        .expect("the session the head is in");
    let store = Arc::new(Store::default());
    registry.set_source(store.clone());

    let socket = socket_in("server");
    let server = serve_registry(registry.clone(), &socket).expect("bind");
    let (mut head, hello, mut reader) =
        HeadClient::attach(&socket, "s-here", 0, "tui", "operator", Caps::default())
            .expect("attach");
    assert!(matches!(hello, ServerFrame::Hello { .. }), "{hello:?}");

    let id = head
        .archive_session("s-away", "the question was answered")
        .expect("send the archive");
    let answer = until(&mut reader, |f| matches!(f, ServerFrame::Sessions { .. }));
    match answer {
        ServerFrame::Sessions {
            created, current, ..
        } => {
            assert_eq!(created, None, "an archive creates nothing: {id}");
            assert_eq!(current, "s-here", "the head stays where it was");
        }
        other => panic!("expected the new list, got {other:?}"),
    }

    assert_eq!(
        store.writes(),
        vec![(
            "s-away".to_string(),
            true,
            "the question was answered".to_string()
        )],
        "the row carries the reason the archiver gave"
    );
    assert!(
        registry.get("s-away").is_none(),
        "a live session is CLOSED by the archive: an archived conversation is not one to keep \
         typing into"
    );
    assert!(
        registry.get("s-here").is_some(),
        "one session archived is not the daemon's others"
    );

    drop(reader);
    drop(head);
    server.shutdown();
}

/// A daemon with no store has nowhere to write the archive, and says so rather than answering with
/// a list that looks as if something happened.
#[test]
fn a_daemon_with_no_store_refuses_to_archive() {
    let registry = Registry::new();
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("a session");
    let why = registry
        .archive("s-1", "because")
        .expect_err("no store, no archive");
    assert!(why.contains("no store"), "{why}");
    assert!(
        registry.get("s-1").is_some(),
        "a refused archive must not close the session as a side effect"
    );
}

/// Unarchiving is the row alone: the session is not live and is not reopened, because opening it is
/// what a head does next.
#[test]
fn unarchiving_writes_the_row_and_reopens_nothing() {
    let registry = Registry::new();
    let store = Arc::new(Store::default());
    registry.set_source(store.clone());
    registry.unarchive("s-old").expect("the store's write");
    assert_eq!(
        store.writes(),
        vec![("s-old".to_string(), false, String::new())],
        "an unarchive clears the stamp and carries no reason"
    );
    assert!(
        registry.get("s-old").is_none(),
        "and it does not seat a hub for it"
    );
}

/// The first Done-when line, on the daemon's side: the archive is the STORE's, so a daemon that is
/// restarted, or a head that was not running when it was made, still answers with the row - in the
/// archived half of the list, which is the group a head draws.
#[test]
fn the_list_carries_the_archived_half_a_restarted_daemon_reads() {
    let registry = Registry::new();
    registry.set_source(Arc::new(KeptStore));
    let live = registry.list();
    assert!(
        live.iter().all(|s| s.session_id != "s-away"),
        "an archived session is not in the list: {live:?}"
    );
    let archived = registry.list_archived();
    assert_eq!(archived.len(), 1, "and it is kept in the other half");
    assert_eq!(archived[0].session_id, "s-away");
    assert_eq!(archived[0].title, "the one put away");
    assert_eq!(archived[0].stored_items, 3, "with its rows counted");
    assert!(
        !archived[0].live,
        "an archived session is not one a daemon is holding: the archive closes it"
    );
}

/// `/close ID` closing a session that is not the one the head is in: the answer is the LIST, because
/// which sessions are live is what the row says, and the session the head stands in is untouched.
#[test]
fn closing_a_named_session_leaves_the_one_you_are_in() {
    let registry = Registry::new();
    registry
        .create("s-away", "the one to close", SessionWiring::default())
        .expect("a session to close");
    registry
        .create("s-here", "the one still open", SessionWiring::default())
        .expect("the session the head is in");
    registry.set_source(Arc::new(Store::default()));

    let socket = socket_in("named-close");
    let server = serve_registry(registry.clone(), &socket).expect("bind");
    let (mut head, hello, mut reader) =
        HeadClient::attach(&socket, "s-here", 0, "tui", "operator", Caps::default())
            .expect("attach");
    assert!(matches!(hello, ServerFrame::Hello { .. }), "{hello:?}");

    head.close_named("s-away", "operator", "the row was finished")
        .expect("send the close");
    let answer = until(&mut reader, |f| {
        matches!(
            f,
            ServerFrame::Sessions { .. } | ServerFrame::Rejected { .. }
        )
    });
    match answer {
        ServerFrame::Sessions { current, .. } => {
            assert_eq!(current, "s-here", "the head stays where it was");
        }
        other => panic!("a named close answers with the list, got {other:?}"),
    }
    assert!(
        registry.get("s-away").is_none(),
        "the named session is closed"
    );
    assert!(
        registry.get("s-here").is_some(),
        "and the session the head is standing in is not"
    );

    drop(reader);
    drop(head);
    server.shutdown();
}
