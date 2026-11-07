//! A held session's turn end reaches the client as one channel notification, over a real
//! socket and `Registry`: said once while the session stays quiet, and again for the next turn.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use letibot_bridge::daemon;
use letibot_bridge::head::Head;
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::testing;
use letibot_transcript::TranscriptItem;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SESSION: &str = "s-push-events";

fn turn(hub: &Hub, id: &str, text: &str) {
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(vec![Waiting::Turn]),
    });
    hub.publish(testing::turn_started(id));
    hub.publish(SessionEvent::TranscriptContent {
        item_id: format!("i-{id}"),
        item: Box::new(TranscriptItem::Assistant {
            text: text.to_string(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    });
    hub.publish(testing::turn_finished(id));
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(Vec::new()),
    });
}

#[test]
fn a_turn_end_is_pushed_once_and_the_next_turn_again() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-mcp-push-{}", std::process::id()));
    let _dir = TempDir(root.clone());
    for d in ["config", "data", "worktree"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let socket = root.join("daemon.sock");
    // SAFETY: this binary runs one test, and these are set before the socket or the head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
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
    letibot_bridge::push::watch(&head, tx);

    turn(&hub, "t1", "first answer");
    let note = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the turn end is pushed");
    assert_eq!(note["method"], "notifications/claude/channel", "{note}");
    assert_eq!(note["params"]["meta"]["event"], "turn_ended", "{note}");
    assert_eq!(note["params"]["meta"]["session"], SESSION, "{note}");
    let content = note["params"]["content"].as_str().unwrap_or("");
    assert!(content.contains("first answer"), "{content}");

    // Quiet is said once, not once a second.
    assert!(
        rx.recv_timeout(Duration::from_secs(4)).is_err(),
        "a quiet session was pushed twice"
    );

    turn(&hub, "t2", "second answer");
    let note = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the next turn end is pushed");
    let content = note["params"]["content"].as_str().unwrap_or("");
    assert!(content.contains("second answer"), "{content}");
}
