//! What a client misses is pushed once when it comes back, and a bridge restarted on the same
//! folder says nothing it already said: the channel is a cursor, not a live feed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_bridge::head::Head;
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::testing;
use letibot_transcript::TranscriptItem;
use serde_json::Value;

/// Generous: every wait here is for something a correct build produces in a second or two.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long a channel stays watched for a message that must not come. Longer than the head's
/// start grace, so a re-attaching head has had its say by then.
const QUIET: Duration = Duration::from_secs(8);

const SESSION: &str = "s-push-replay";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One turn with an answer in it, so two turns are two different events. The row is
/// `TranscriptAppended` and not a bare `TranscriptContent`: the view a head attaching LATER is
/// handed builds its rows from the append, and the body rides it there.
fn turn(hub: &Hub, id: &str, text: &str) {
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(vec![Waiting::Turn]),
    });
    hub.publish(testing::turn_started(id));
    hub.publish(SessionEvent::TranscriptAppended {
        item_id: format!("i-{id}"),
        kind: "assistant".into(),
        ledger_head: "beef".into(),
        item: Some(Box::new(TranscriptItem::Assistant {
            text: text.to_string(),
            tool_calls: Vec::new(),
            truncated: false,
        })),
        body_bytes: 0,
    });
    hub.publish(testing::turn_finished(id));
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(Vec::new()),
    });
}

fn kind(note: &Value) -> &str {
    note["params"]["meta"]["event"].as_str().unwrap_or("")
}

fn content(note: &Value) -> &str {
    note["params"]["content"].as_str().unwrap_or("")
}

fn one(rx: &Receiver<Value>) -> Value {
    rx.recv_timeout(PATIENCE).expect("an event is pushed")
}

/// Wait for something the channel's own state says has happened, without guessing at a sleep.
fn until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !cond() {
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}, and it did not happen"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn what_happened_while_nobody_was_reading_is_pushed_once_on_reconnect() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-mcp-push-replay-{}", std::process::id()));
    let _dir = TempDir(root.clone());
    for d in ["config", "data", "worktree", "run"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let socket = root.join("daemon.sock");
    // SAFETY: this binary runs one test, and these are set before the socket or the head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        // The per-session channel is kept under the rundir; a test keeps it under its own root.
        std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::remove_var("LETIBOT_STORE");
    }
    let registry = Registry::new();
    registry
        .create(SESSION, "", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let hub = registry.get(SESSION).expect("the session's hub");
    let head = Arc::new(
        Head::attach(
            &daemon::resolve(&root.join("worktree")).expect("the daemon resolves"),
            SESSION,
        )
        .expect("the head attaches"),
    );
    let (tx, rx) = mpsc::channel();
    let outbox = letibot_bridge::push::watch(&head);
    outbox.attach(tx);

    turn(&hub, "t1", "first answer");
    let note = one(&rx);
    assert_eq!(kind(&note), "turn_ended", "{note}");
    assert!(content(&note).contains("first answer"), "{note}");

    // The client goes away: its end of the channel is dropped, so the next event cannot be taken.
    drop(rx);
    turn(&hub, "t2", "second answer");
    until("the turn that nobody was reading is recorded", || {
        outbox.waiting() == 1
    });

    // It comes back, and is owed the one event it missed - and only that one.
    let (tx, rx) = mpsc::channel();
    outbox.attach(tx);
    let note = one(&rx);
    assert_eq!(kind(&note), "turn_ended", "{note}");
    assert!(
        content(&note).contains("second answer"),
        "the replayed event is the one that happened while nobody was reading: {note}"
    );
    assert_eq!(outbox.waiting(), 0, "the record was handed over once");
    assert!(
        rx.recv_timeout(Duration::from_secs(3)).is_err(),
        "the replay said an event twice"
    );

    // A bridge restarted on this folder: the same session, a new head and a new watcher, and the
    // channel's file is the only thing they share.
    drop(head);
    let head = Arc::new(
        Head::attach(
            &daemon::resolve(&root.join("worktree")).expect("the daemon resolves"),
            SESSION,
        )
        .expect("the head attaches again"),
    );
    let restarted = letibot_bridge::push::watch(&head);
    let (tx, rx) = mpsc::channel();
    restarted.attach(tx);
    assert_eq!(
        restarted.cursor(),
        outbox.cursor(),
        "a restart resumes the cursor the first run left"
    );
    if let Ok(note) = rx.recv_timeout(QUIET) {
        panic!("a restarted bridge re-pushed what it had already pushed: {note}");
    }
}
