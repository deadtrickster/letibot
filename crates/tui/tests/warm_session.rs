//! **A head that asks a running daemon for a stored session must end up holding its rows.**
//!
//! MEASURED 2026-10-26 ~14:10 on macOS: the operator's daemon for `~/Projects/stroppy-io`
//! was up and seated on one session; they ran `leticode --session s-1793…`, a session the
//! store had with 166 rows. The head drew that session's title in its status line and an
//! **empty transcript**, and a prompt typed into it sat `queued`.
//!
//! The daemon's half was fine: its log says `s-1793017817134441000 · resumed 166 row(s),
//! 537946 tokens` and the prompt that followed was appended as seq 166 of a conversation
//! the model went on talking about. What failed is the head's half — the path from
//! *"ask for a session this daemon does not hold"* to *"draw the conversation in it"*.
//!
//! # The path, in five frames
//!
//! 1. `ResumeSession { id }` — the head asks (what `leticode --session ID` becomes, via
//!    `letibot-tui --resume ID`).
//! 2. `Sessions { created: Some(id) }` — the daemon registered a hub for it and rung the
//!    worker to open it.
//! 3. `Switch { id }` — the head goes there, because a created session is what
//!    `want_new_session` means.
//! 4. `Hello { snapshot }` — **and here the snapshot is cut from a hub whose restore has
//!    likely not started**: `Work::Open` runs on the worker, behind whatever turn it is
//!    running at that moment.
//! 5. the restored rows, as `TranscriptAppended` + `TranscriptContent`, published when the
//!    worker gets to the resume.
//!
//! So the head's ability to draw the conversation does not depend on the snapshot: it
//! depends on those two events landing on a session it is attached to, and on it holding
//! them. That is what this file measures, with a real `Registry` and a real socket.
//!
//! # Why the rows are published by the test and not by a `Harness`
//!
//! A `Harness` is `letibot-harnessd`'s, and this crate is below it (the daemon links the
//! head's protocol, not the other way round). What the head sees is a hub, and a hub is a
//! registry's — so the restore is stood up as the events it publishes, at the two moments
//! it can publish them: before the head switches (the restore won the race) and after
//! (the worker was busy, which is the operator's case). The end-to-end half — a real
//! store, a real resume, a real prompt during it — is `letibot-harnessd`'s
//! `tests/warm_open.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::Hub;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_sessionlog::server::serve_registry;
use letibot_transcript::{TranscriptItem, UserPart};

use letibot_tui::app::App;
use letibot_tui::driver::Link;
use letibot_tui::ui::render::RenderConfig;

/// The daemon's own first session — what a bare attach lands on.
const SEATED: &str = "s-seated";
/// The session that is only in the store: the one the operator asked for.
const STORED: &str = "s-stored";

fn socket_path(tag: &str) -> PathBuf {
    let mut p = letibot_sessionlog::testing::scratch_root();
    p.push(format!("letibot-warm-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn app() -> App {
    App::new(RenderConfig {
        width: 100,
        color: false,
        ..RenderConfig::default()
    })
}

fn step(link: &mut Link, a: &mut App) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, (100, 30), &[], &[], &mut sink);
}

fn screen(a: &mut App) -> String {
    a.screen(100, 30).join("\n")
}

/// Tick until `poll` answers, or fail. Frames sit in the pump's channel until a `tick`
/// drains them, so a wait that did not tick would be asserting a fact arrives by itself.
fn until<T>(
    link: &mut Link,
    a: &mut App,
    what: &str,
    mut poll: impl FnMut(&mut App) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        step(link, a);
        if let Some(v) = poll(a) {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}; screen was:\n{}", screen(a));
}

/// **What the store would answer, without a store.** The row a `ResumeSession` reads to
/// decide the session is resumable at all, and the title and wiring that come in with it.
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

fn brief(session_id: &str, title: &str, items: u32) -> StoredBrief {
    StoredBrief {
        session_id: session_id.into(),
        title: title.into(),
        items,
        last_activity_ms: 1_788_990_321_957,
        wiring: SessionWiring::default(),
        parent_session_id: None,
        context_tokens: Some(44_700),
        context_cached: Some(40_000),
        stored_end: None,
        nags_paused: None,
        waits_on_operator: 0,
        slug: String::new(),
    }
}

/// Two sessions in one registry, served on a real socket: the daemon's seated one, and the
/// stored one a head may ask for.
fn start(
    tag: &str,
) -> (
    Arc<Registry>,
    Arc<Hub>,
    letibot_sessionlog::server::ServerHandle,
) {
    let registry = Registry::new();
    registry
        .create(
            SEATED,
            "the session the daemon is in",
            SessionWiring::default(),
        )
        .expect("the seated session");
    // The store has BOTH: a live session has a store row too. Only `STORED` lacks a hub,
    // which is exactly the state `ResumeSession` is for.
    registry.set_source(Arc::new(Disk(Mutex::new(vec![
        brief(SEATED, "the session the daemon is in", 3),
        brief(STORED, "the pptx conversation", 166),
    ]))));
    let path = socket_path(tag);
    let server = serve_registry(registry.clone(), &path).expect("the socket binds");
    (
        registry.clone(),
        registry.get(SEATED).expect("the seated hub"),
        server,
    )
}

/// **The restore was published after the head switched** — the operator's case, where the
/// worker was running another session's turn and the resume waited behind it.
///
/// This is the frame the operator saw, and the whole of the fix is in the two assertions
/// before the rows arrive: the screen says the conversation is IN the store and has not been
/// loaded, and it does not say the session has said nothing.
#[test]
fn a_session_whose_rows_are_still_being_restored_says_so_rather_than_looking_empty() {
    let (registry, _seated, server) = start("late");
    let mut link = Link::open(server.path(), "", 0, "tui", "dead").expect("attach");
    let mut a = app();
    until(&mut link, &mut a, "the first Hello", |a| {
        (a.session_id() == SEATED).then_some(())
    });

    // 1–3: the ask, the creation, the switch.
    a.request_session(STORED);
    until(&mut link, &mut a, "the switch to the stored session", |a| {
        (a.session_id() == STORED).then_some(())
    });
    let hub = registry
        .get(STORED)
        .expect("ResumeSession registered a hub");
    assert!(
        hub.snapshot().items.is_empty(),
        "so far nothing has been published into it, which is the state the operator met"
    );

    let empty = screen(&mut a);
    assert!(
        !empty.contains("has said nothing yet"),
        "the list this head was handed says the session holds 166 rows, so the empty-session \
         banner is a false claim about it:\n{empty}"
    );
    assert!(
        empty.contains("restore") && empty.contains("166"),
        "and the screen says what IS true: the rows exist and have not been loaded:\n{empty}"
    );

    // 5: the restore, as the events `Harness::open` publishes them.
    publish_rows(&hub, "stroppy_final_final");
    until(&mut link, &mut a, "the restored rows on the screen", |a| {
        screen(a).contains("stroppy_final_final").then_some(())
    });
    let drawn = screen(&mut a);
    assert!(
        !drawn.contains("have not been \n") && !drawn.contains("are in the store and have not"),
        "and the line goes when the conversation arrives:\n{drawn}"
    );

    link.detach();
}

/// **A session the store says has nothing** keeps the ordinary banner: the new line is a fact
/// about a disagreement between two sources, not a second way to greet an empty session.
#[test]
fn a_session_with_no_stored_rows_still_draws_the_ordinary_banner() {
    let (registry, _seated, server) = start("empty");
    registry.set_source(Arc::new(Disk(Mutex::new(vec![
        brief(SEATED, "the session the daemon is in", 3),
        brief(STORED, "a session the store has no rows for", 0),
    ]))));
    let mut link = Link::open(server.path(), "", 0, "tui", "dead").expect("attach");
    let mut a = app();
    until(&mut link, &mut a, "the first Hello", |a| {
        (a.session_id() == SEATED).then_some(())
    });
    a.request_session(STORED);
    until(&mut link, &mut a, "the switch", |a| {
        (a.session_id() == STORED).then_some(())
    });
    let shown = screen(&mut a);
    assert!(
        shown.contains("has said nothing yet"),
        "nothing is missing here, so the banner is true and stays:\n{shown}"
    );
    link.detach();
}

/// **The restore was published before the head switched** — the row set is in the hub's
/// view, so the switch's `Hello` carries it as a snapshot.
#[test]
fn rows_restored_before_the_switch_arrive_in_the_snapshot() {
    let (registry, _seated, server) = start("early");
    // The session exists and its rows are in the hub before any head asks for it.
    registry
        .create(STORED, "a stored conversation", SessionWiring::default())
        .expect("the stored session");
    let hub = registry.get(STORED).expect("its hub");
    publish_rows(&hub, "stroppy_final_final");

    let mut link = Link::open(server.path(), "", 0, "tui", "dead").expect("attach");
    let mut a = app();
    until(&mut link, &mut a, "the first Hello", |a| {
        (a.session_id() == SEATED).then_some(())
    });
    a.request_session(STORED);
    until(&mut link, &mut a, "the switch to the stored session", |a| {
        (a.session_id() == STORED).then_some(())
    });
    let shown = screen(&mut a);
    assert!(
        shown.contains("stroppy_final_final"),
        "the switch's snapshot carries the rows:\n{shown}"
    );

    link.detach();
}

/// One operator row and one assistant row, published exactly as `Harness::republish_after`
/// publishes a restored conversation.
fn publish_rows(hub: &Arc<Hub>, text: &str) {
    let rows: Vec<(&str, TranscriptItem)> = vec![
        (
            "t.0",
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: format!("look at {text}"),
                }],
            },
        ),
        (
            "t.1",
            TranscriptItem::Assistant {
                text: format!("I read {text}"),
                tool_calls: Vec::new(),
                truncated: false,
            },
        ),
    ];
    for (i, (id, item)) in rows.iter().enumerate() {
        hub.publish(SessionEvent::TranscriptAppended {
            item_id: (*id).to_string(),
            kind: if i == 0 { "user" } else { "assistant" }.to_string(),
            ledger_head: "0".repeat(64),
        });
        hub.record_item(id, item.clone());
    }
    // And the completion `republish_after` publishes unconditionally.
    hub.publish(SessionEvent::Filling {
        what: "restoring the stored conversation".into(),
        unit: "rows".into(),
        done: 2,
        total: 2,
    });
}
