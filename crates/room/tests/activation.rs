//! A coordinator on a listener a socket manager holds (launchd, systemd): stopped and served
//! again on the same fd, a connected client reconnects by itself and finds its queue and chat.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::lease::CoordinatorLease;
use letibot_room::server::{Server, now_ms};
use letibot_room::{Request, Response, Room, Speaker};

const LEASE: u64 = 90_000;
const LONG_SWEEP: u64 = 3_600_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        // Short: a unix socket path is limited to about a hundred bytes.
        let p = std::env::temp_dir().join(format!("lb-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What an activated coordinator does at start: lease, then serve on the manager's fd.
fn coordinator(s: &Scratch, held: &UnixListener, owner: &str) -> (Server, CoordinatorLease) {
    let lease = CoordinatorLease::owned_by(s.0.join("room.lease"), owner);
    lease.acquire(now_ms()).expect("the lease");
    let server = Server::with_listener(
        held.try_clone().expect("a dup of the manager's fd"),
        &s.0.join("room.sock"),
        Room::with_lease_ms(LEASE),
        LONG_SWEEP,
    )
    .expect("a coordinator");
    server
        .with_chat_store(&s.0.join("room.chat"))
        .with_item_store(&s.0.join("room.items"))
        .with_lease(lease.clone());
    (server, lease)
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_session_reconnects_across_a_restart_on_the_same_listener_and_loses_nothing() {
    let s = Scratch::new("act");
    let path = s.0.join("room.sock");
    // The manager's socket: bound once, outliving every coordinator served on it.
    let held = UnixListener::bind(&path).expect("the manager binds");
    let (first, _lease) = coordinator(&s, &held, "box/activated/1");

    // A stray coordinator started by hand is refused by the live lease before it binds.
    let stray = CoordinatorLease::owned_by(s.0.join("room.lease"), "box/stray/2");
    let held_by = stray
        .acquire(now_ms())
        .expect_err("a live lease refuses the stray");
    assert_eq!(held_by.holder.owner, "box/activated/1");

    let c = Client::connect(&path, Speaker::top_level("s-a"), Arc::new(|_: &_| {}))
        .expect("the session connects");
    let item = match c
        .request(&Request::Post {
            id: "p1".into(),
            tag: "build".into(),
            body: "kept".into(),
        })
        .expect("post")
    {
        Response::Posted { item, .. } => item.id,
        other => panic!("{other:?}"),
    };
    let said = match c
        .request(&Request::Say {
            id: "m1".into(),
            room: String::new(),
            text: "before the restart".into(),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
        })
        .expect("say")
    {
        Response::Said { message, .. } => message,
        other => panic!("{other:?}"),
    };
    let before = c.welcome().instance;
    assert_eq!(first.connections(), 1);

    first.stop();
    assert!(
        path.exists(),
        "a stop leaves the manager's socket file alone"
    );
    drop(first);
    std::thread::sleep(Duration::from_millis(5));

    // The orderly stop gave the lease back, so the next start is fresh.
    let (second, _lease) = coordinator(&s, &held, "box/activated/3");
    assert_eq!(
        CoordinatorLease::owned_by(s.0.join("room.lease"), "box/activated/3")
            .read()
            .map(|h| h.owner),
        Some("box/activated/3".to_string())
    );
    wait_for("the session's redial", || {
        c.connected() && c.welcome().instance != before && second.connections() == 1
    });

    match c
        .request(&Request::Claim {
            id: "c1".into(),
            tag: "build".into(),
        })
        .expect("claim after the restart")
    {
        Response::Claimed { item: got, .. } => assert_eq!(got.id, item),
        other => panic!("the queue must survive: {other:?}"),
    }
    match c
        .request(&Request::Read {
            room: said.room.clone(),
            since: 0,
            last: None,
            thread: None,
            before: None,
        })
        .expect("read after the restart")
    {
        Response::History(h) => assert!(
            h.rows.iter().any(|m| m.text == "before the restart"),
            "{:?}",
            h.rows
        ),
        other => panic!("{other:?}"),
    }
    second.stop();
}
