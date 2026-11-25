//! `Search`: conversation rows over one session or the whole project, and the `Searched` answer.
//! The store is the daemon's, so the wire's half is what this proves: the question the head asks,
//! the answer it is owed, and the sentence a daemon with no store answers with instead of nothing.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use letibot_sessionlog::protocol::{
    Caps, ClientFrame, PROTOCOL_VERSION, SearchAnswer, SearchHit, ServerFrame,
};
use letibot_sessionlog::registry::{Registry, SessionSource, StoredBrief};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

/// A source with three conversations, two of them the operator's own and one archived: the facts
/// the daemon's own store would decide, handed back as they are decided there.
struct Conversations;

impl SessionSource for Conversations {
    fn list(&self) -> Vec<StoredBrief> {
        Vec::new()
    }

    fn search(
        &self,
        workspace: &str,
        session: Option<&str>,
        _query: &str,
        all: bool,
        from: usize,
        count: usize,
    ) -> Option<SearchAnswer> {
        // The session's own scope answers one hit and the project's two, so an answer to the wrong
        // question is visible in what comes back rather than in a count. `all` adds the one a
        // subagent wrote, which is the only difference the toggle makes in the store.
        let mut hits: Vec<SearchHit> = match session {
            Some(id) => vec![a_hit(id, "row in the open conversation", 3, "mine")],
            None => vec![
                a_hit("s-main", "row in the main conversation", 7, "mine"),
                a_hit("s-old", "row in an archived one", 2, "agent"),
            ],
        };
        if all {
            hits.push(a_hit("s-child", "row a subagent wrote", 1, "tool"));
        }
        // The project is the asking session's: another folder's conversations are not read.
        hits.retain(|h| h.folder == workspace);
        let hits: Vec<SearchHit> = hits.into_iter().skip(from).take(count).collect();
        Some(SearchAnswer {
            hits,
            more: false,
            scanned: 12,
            sessions: 2,
        })
    }
}

fn a_hit(session: &str, excerpt: &str, row: usize, role: &str) -> SearchHit {
    SearchHit {
        session: session.into(),
        title: format!("{session} title"),
        folder: "/w/one".into(),
        at_ms: 1_700_000_000_000,
        role: role.into(),
        tool: String::new(),
        row,
        item_id: format!("{session}#t0.{row}"),
        excerpt: excerpt.into(),
    }
}

fn attach(registry: &Arc<Registry>) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    let _server = std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);
    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-main".into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "test".into(),
        caps: Caps::default(),
    })
    .expect("attach");
    assert!(matches!(
        r.read::<ServerFrame>().expect("hello"),
        ServerFrame::Hello { .. }
    ));
    (w, r)
}

fn start() -> Arc<Registry> {
    let registry = Registry::new();
    registry
        .create(
            "s-main",
            "the main conversation",
            letibot_sessionlog::registry::SessionWiring {
                workspace: "/w/one".into(),
                ..Default::default()
            },
        )
        .expect("create");
    registry.set_source(Arc::new(Conversations));
    registry
}

/// Answer one `Search`, skipping the session's own live events on the way: they share the stream.
fn search(
    w: &mut FrameWriter<UnixStream>,
    r: &mut FrameReader<UnixStream>,
    query: &str,
    session: Option<&str>,
    all: bool,
) -> (String, Option<String>, Vec<SearchHit>, bool, Option<String>) {
    w.write(&ClientFrame::Search {
        query: query.into(),
        session: session.map(str::to_string),
        all,
        from: 0,
        count: 40,
    })
    .expect("search");
    loop {
        match r.read::<ServerFrame>().expect("an answer") {
            ServerFrame::Searched {
                query,
                session,
                hits,
                more,
                why,
                ..
            } => return (query, session, hits, more, why),
            ServerFrame::Event(_) => continue,
            other => panic!("expected Searched, got {other:?}"),
        }
    }
}

/// The project's scope answers every conversation's hits - the archived one's included - and the
/// query is echoed so two searches in flight cannot answer each other.
#[test]
fn a_project_search_answers_every_conversation_the_store_reads() {
    let registry = start();
    let (mut w, mut r) = attach(&registry);
    let (query, session, hits, more, why) = search(&mut w, &mut r, "cache", None, false);
    assert_eq!(query, "cache");
    assert_eq!(session, None);
    assert!(why.is_none(), "{why:?}");
    assert!(!more);
    let ids: Vec<&str> = hits.iter().map(|h| h.session.as_str()).collect();
    assert_eq!(ids, ["s-main", "s-old"], "the archive is read: {ids:?}");
    assert_eq!(hits[1].role, "agent", "whose words matched travels");
    assert_eq!(
        hits[0].row, 7,
        "the row ordinal travels, so a head can open it"
    );
}

/// A session's own scope is the same request with `session` named: the store reads one
/// conversation's rows, and the answer says which one it was about.
#[test]
fn a_session_search_names_the_conversation_it_read() {
    let registry = start();
    let (mut w, mut r) = attach(&registry);
    let (_, session, hits, _, _) = search(&mut w, &mut r, "cache", Some("s-other"), false);
    assert_eq!(session.as_deref(), Some("s-other"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row, 3);
}

/// A daemon with no store says so: a head that drew *no hits* would be saying the project has
/// none, and the two are different answers.
#[test]
fn a_daemon_with_no_store_says_it_cannot_search() {
    let registry = Registry::new();
    registry
        .create("s-main", "one", Default::default())
        .expect("create");
    let (mut w, mut r) = attach(&registry);
    let (_, _, hits, _, why) = search(&mut w, &mut r, "cache", None, false);
    assert!(hits.is_empty());
    let why = why.expect("a daemon with no store says why");
    assert!(why.contains("no session store"), "{why}");
}
