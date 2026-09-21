//! Reaching a session that is **not in the daemon yet**, over a real socket.
//!
//! `sessions.rs` covers a daemon that already holds every session a head can name.
//! This covers the case that produced the report — *"no session management"* — where
//! the conversation the operator wants was had by a daemon that has since exited, and
//! the only trace of it is on disk.
//!
//! # Why a fake source rather than a `Store`
//!
//! `letibot-sessionlog` does not depend on `letibot-tokencore` and must not start:
//! the log and the token ledger are two strands that meet in `harnessd`, and a
//! dependency here would make every head binary link a SQLite. So [`SessionSource`]
//! is a trait, and this file implements it over a `Vec` — which also makes the two
//! failure modes that matter cheap to arrange: a source that has never heard of an
//! id, and a registry with no source at all.
//!
//! # What is asserted, and why each one is here
//!
//! 1. **A stored session appears in the list, marked as not live.** A picker that
//!    showed it identically to a live one would offer a one-keystroke switch for a
//!    row that needs two frames, and the operator would find out by pressing enter.
//! 2. **`ResumeSession` brings it in, and is idempotent.** "Resume the one I am
//!    already in" is a thing `letibot --continue` asks for on the most ordinary run
//!    there is, and a refusal there would send it down an error path.
//! 3. **An id nobody has heard of is refused by name, and by a *different* name
//!    than an id the daemon merely does not hold.** Those two send an operator to
//!    two different places.
//! 4. **A rename with nowhere to store it is refused rather than accepted.** A name
//!    that lives only in a daemon's memory comes back as the old name after a
//!    restart, with nothing having said so.
//! 5. **A creation does not displace a command in arrival order.** The worker's
//!    wake queue carries both, and §13.2's promise that two heads are served in the
//!    order they pressed enter is not a promise about commands only when nobody is
//!    making sessions.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::protocol::{
    Caps, REJECT_NOT_IN_STORE, REJECT_UNKNOWN_SESSION, ServerFrame,
};
use letibot_sessionlog::registry::{
    Registry, SessionSource, SessionWiring, StoredBrief, Work,
};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-resume-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn wiring(workspace: &str) -> SessionWiring {
    SessionWiring {
        model: "qwen-3.8-flash-next".into(),
        dialect: "qwen3.8".into(),
        endpoint: "127.0.0.1:8080".into(),
        workspace: workspace.into(),
    }
}

/// What a store would answer, without one.
struct Disk(Mutex<Vec<StoredBrief>>);

impl SessionSource for Disk {
    fn list(&self) -> Vec<StoredBrief> {
        self.0.lock().unwrap().clone()
    }
    fn set_title(&self, session_id: &str, title: &str) -> Result<(), String> {
        let mut g = self.0.lock().unwrap();
        match g.iter_mut().find(|s| s.session_id == session_id) {
            Some(s) => {
                s.title = title.to_string();
                Ok(())
            }
            None => Err(format!("no such session {session_id}")),
        }
    }
}

fn start(tag: &str) -> (Arc<Registry>, ServerHandle) {
    let r = Registry::new();
    r.create("live", "", wiring("/home/dead")).unwrap();
    // Both of them: a live session has a store row too, which is what makes a
    // rename of the one you are in durable. Only `s-old` lacks a hub.
    r.set_source(Arc::new(Disk(Mutex::new(vec![
        StoredBrief {
            session_id: "live".into(),
            title: String::new(),
            items: 3,
            last_activity_ms: 1_788_990_321_957,
            wiring: wiring("/home/dead"),
            parent_session_id: None,
            context_tokens: Some(44_700),
            context_cached: Some(40_000),
        },
        StoredBrief {
            session_id: "s-old".into(),
            title: "the rano question".into(),
            items: 56,
            last_activity_ms: 1_788_987_703_152,
            wiring: wiring("/home/dead/Projects/rano"),
            parent_session_id: None,
            context_tokens: Some(12_000),
            context_cached: None,
        },
    ]))));
    let h = serve_registry(r.clone(), socket_path(tag)).expect("bind");
    (r, h)
}

fn until(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    mut f: impl FnMut(&ServerFrame) -> bool,
) -> ServerFrame {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(frame) => {
                let frame = frame.frame();
                if f(&frame) {
                    return frame;
                }
            }
            Err(_) => break,
        }
    }
    panic!("the frame never arrived");
}

#[test]
fn a_stored_session_is_listed_and_is_marked_as_not_live() {
    let (registry, _server) = start("list");
    let rows = registry.list();
    assert_eq!(rows.len(), 2, "one live and one on disk: {rows:?}");

    let live = rows.iter().find(|b| b.session_id == "live").unwrap();
    assert!(live.live);

    let old = rows.iter().find(|b| b.session_id == "s-old").unwrap();
    assert!(!old.live, "a session with no hub is not live");
    assert_eq!(old.title, "the rano question");
    assert_eq!(
        old.stored_items, 56,
        "the store's count, not the view's — a head that read `status.items` here \
         would see 0 and call a 56-row conversation empty"
    );
    assert_eq!(old.status.items, 0, "there is no hub, so there is no view to cut");
    assert_eq!(
        old.wiring.workspace, "/home/dead/Projects/rano",
        "a stored session's workspace is its own, not the daemon's"
    );

    // Live rows first: a switch reaches them in one keystroke and a stored one needs
    // two, and a list that interleaved them would put the two next to each other
    // with nothing to say which is which.
    assert!(rows[0].live && !rows[1].live, "{rows:?}");
}

#[test]
fn the_brief_carries_the_last_turns_context_from_the_store_row() {
    let (registry, _server) = start("context");
    let rows = registry.list();

    // The stored-only arm: the row is the whole of what is known, and the context
    // is on it. A head that attaches after a restart reads the number from here —
    // the snapshot it is sent has no turn state to read it from.
    let old = rows.iter().find(|b| b.session_id == "s-old").unwrap();
    assert_eq!(old.context_tokens, Some(12_000));
    assert_eq!(old.context_cached, None);

    // The live arm: the store's row is the source, so a live session's brief
    // carries the same fact a head would get from the snapshot's turn state while
    // the daemon holds it — and the one that survives the restart.
    let live = rows.iter().find(|b| b.session_id == "live").unwrap();
    assert_eq!(live.context_tokens, Some(44_700));
    assert_eq!(live.context_cached, Some(40_000));
}

#[test]
fn resume_brings_a_stored_session_in_and_saying_it_twice_is_not_an_error() {
    let (registry, server) = start("resume");
    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "live", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    client.resume_session("s-old").unwrap();
    let f = until(&rx, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions { created, .. } = f else {
        unreachable!()
    };
    assert_eq!(
        created.as_deref(),
        Some("s-old"),
        "answered like a creation, so a head switches to it by the same path"
    );
    assert!(registry.get("s-old").is_some(), "it has a hub now");
    assert_eq!(
        registry.wiring("s-old").workspace,
        "/home/dead/Projects/rano",
        "the session's own workspace came in with it, not this connection's"
    );
    assert!(
        registry.resumable("s-old").is_none(),
        "`resumable` answers `do I need to bring it in`, and it is already here"
    );

    // Again. `letibot --continue` asks for the session it is already in on the most
    // ordinary run there is.
    client.resume_session("s-old").unwrap();
    let f = until(&rx, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions { created, .. } = f else {
        unreachable!()
    };
    assert_eq!(created.as_deref(), Some("s-old"), "idempotent, not refused");

    let _ = client.detach();
    drop(client);
    let _ = pumping.join();
}

#[test]
fn an_id_nobody_has_heard_of_is_refused_differently_from_one_that_is_merely_not_held() {
    let (_registry, server) = start("miss");
    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "live", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    client.resume_session("s-typo").unwrap();
    let f = until(&rx, |f| matches!(f, ServerFrame::Rejected { .. }));
    let ServerFrame::Rejected { reason, .. } = f else {
        unreachable!()
    };
    assert!(
        reason.contains(REJECT_NOT_IN_STORE),
        "a resume of something that does not exist anywhere must not quietly create \
         a session named after the typo: {reason}"
    );

    // The other refusal, so the two are visibly different sentences.
    client.switch("s-old", 0).unwrap();
    let f = until(&rx, |f| matches!(f, ServerFrame::Rejected { .. }));
    let ServerFrame::Rejected { reason: r2, .. } = f else {
        unreachable!()
    };
    assert!(r2.contains(REJECT_UNKNOWN_SESSION), "{r2}");
    assert!(
        !r2.contains(REJECT_NOT_IN_STORE),
        "`this daemon does not hold it` and `nothing has heard of it` send an \
         operator to two different places: {r2}"
    );

    let _ = client.detach();
    drop(client);
    let _ = pumping.join();
}

#[test]
fn a_rename_is_durable_or_it_is_refused() {
    let (registry, _server) = start("rename");
    registry.rename("live", "the cache question").expect("a source is set");
    assert_eq!(
        registry
            .list()
            .iter()
            .find(|b| b.session_id == "live")
            .unwrap()
            .title,
        "the cache question"
    );

    // A registry with nowhere to put a name refuses, rather than accepting one that
    // will be gone after a restart with nothing having said so.
    let bare = Registry::new();
    bare.create("only", "", SessionWiring::default()).unwrap();
    let err = bare.rename("only", "nowhere").expect_err("no source, no rename");
    assert!(err.contains("no store"), "{err}");
    assert_eq!(
        bare.list()[0].title,
        "",
        "and the in-memory half must not have happened either"
    );
}

#[test]
fn making_a_session_does_not_displace_a_command_that_was_submitted_first() {
    // §13.2 promises two heads are served in the order they pressed enter. That is a
    // promise about the worker's wake queue, and a creation that pushed onto the same
    // queue made a later session's first command overtake an earlier prompt —
    // measured, before the bell grew a second lane for opens.
    let registry = Registry::new();
    let a = registry.create("s-a", "", SessionWiring::default()).unwrap();
    let ha = a.attach("tui", "alice", Caps::default(), 0);
    // Drain the open that creating `s-a` queued.
    assert!(matches!(registry.next_work(), Some(Work::Open(id)) if id == "s-a"));

    a.submit(
        &ha.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "first".into(),
        },
    );
    let b = registry.create("s-b", "", SessionWiring::default()).unwrap();
    let hb = b.attach("tui", "bob", Caps::default(), 0);
    b.submit(
        &hb.head_id,
        "c2",
        0,
        CommandKind::Prompt {
            text: "second".into(),
        },
    );

    // The open comes first — a session must be opened before anything runs in it,
    // which is where a resume's chain check lives — and then the two prompts in the
    // order they were submitted.
    assert!(matches!(registry.next_work(), Some(Work::Open(id)) if id == "s-b"));
    match registry.next_work() {
        Some(Work::Command(id, cmd)) => {
            assert_eq!(id, "s-a");
            assert_eq!(cmd.kind, CommandKind::Prompt { text: "first".into() });
        }
        _ => panic!("the first prompt must be served first"),
    }
    match registry.next_work() {
        Some(Work::Command(id, cmd)) => {
            assert_eq!(id, "s-b");
            assert_eq!(cmd.kind, CommandKind::Prompt { text: "second".into() });
        }
        _ => panic!("the second prompt must be served second"),
    }
}

/// **R6: an `oc-` id is created for import, not refused as unknown.**
///
/// `--session oc-<id>` names an opencode conversation that lives in *opencode's*
/// database and nowhere in this daemon's store. Before R6 the `ResumeSession` for it was
/// refused with `no such session in the store` — correct for a typo, wrong for an import.
/// The two are told apart by the namespace mark: `oc-` means *bring opencode's
/// conversation in*, and "this daemon does not hold it" is exactly the state an import
/// starts from. The daemon's open then reads the database into the session it created.
#[test]
fn an_oc_id_is_created_for_import_rather_than_refused_as_unknown() {
    let (registry, server) = start("oc");
    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "live", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    let oc = "oc-ses_f68f5fd80ffe3lS06lxORVNfS9";
    client.resume_session(oc).unwrap();
    let f = until(&rx, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions { created, .. } = f else {
        unreachable!()
    };
    assert_eq!(
        created.as_deref(),
        Some(oc),
        "created, so the daemon's open can read the database into it"
    );
    assert!(
        registry.get(oc).is_some(),
        "the session exists, so a head can attach and watch it fill"
    );

    // **An id that is neither `oc-` nor in the store is still refused by name.** The
    // `oc-` arm must not become "any unknown id creates a session", which would make a
    // typo indistinguishable from an import — the exact confusion the two reject codes
    // exist to keep apart.
    client.resume_session("s-typo").unwrap();
    let f = until(&rx, |f| matches!(f, ServerFrame::Rejected { .. }));
    let ServerFrame::Rejected { reason, .. } = f else {
        unreachable!()
    };
    assert!(reason.contains(REJECT_NOT_IN_STORE), "{reason}");
    assert!(reason.contains("s-typo"), "{reason}");

    let _ = client.detach();
    drop(client);
    let _ = pumping.join();
}
