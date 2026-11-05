//! A `wait` rides over a provider stall and ends at the turn's end, while a quota refusal
//! ends it at once with 40. `--exit-on-stall` restores the stall exit. Real socket and `Registry`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use letibot_bridge::tools::wait_with;
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::testing;
use letibot_transcript::TranscriptItem;

const SESSION: &str = "s-wait-stall";

/// A directory that removes itself.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn trouble(kind: &str) -> SessionEvent {
    SessionEvent::ProviderTrouble {
        kind: kind.into(),
        model: "deepseek/deepseek-chat".into(),
        detail: "test".into(),
        reset_ms: None,
        count: 1,
        switched_to: None,
    }
}

/// Run `wait_with` on a thread, give it time to attach and see a running turn, then publish `during`.
fn waiting(
    worktree: &str,
    hub: &Hub,
    exit_on_stall: bool,
    during: impl FnOnce(&Hub),
) -> std::thread::JoinHandle<(i32, String)> {
    hub.publish(testing::turn_started("t1"));
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(vec![Waiting::Turn]),
    });
    let worktree = worktree.to_string();
    let handle = std::thread::spawn(move || {
        wait_with(
            &worktree,
            SESSION,
            Instant::now() + Duration::from_secs(30),
            exit_on_stall,
        )
    });
    // After the attach: a trouble folded into the snapshot is not news to the wait.
    std::thread::sleep(Duration::from_millis(500));
    during(hub);
    handle
}

fn finish(hub: &Hub) {
    hub.publish(SessionEvent::TranscriptContent {
        item_id: "i-done".into(),
        item: Box::new(TranscriptItem::Assistant {
            text: "all done".into(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    });
    hub.publish(testing::turn_finished("t1"));
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::Quiet,
    });
}

#[test]
fn a_stall_is_ridden_out_and_a_quota_ends_the_wait() {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = TempDir(
        letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-mcp-stall-{}-{n}", std::process::id())),
    );
    let root = dir.0.clone();
    std::fs::create_dir_all(root.join("config")).expect("config dir");
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let socket = root.join("daemon.sock");
    // SAFETY: this binary runs one test, and these are set before the socket or any head exists.
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
    let wt = worktree.display().to_string();

    // A stall, then the answer: the wait ends only at the turn's end.
    let began = Instant::now();
    let h = waiting(&wt, &hub, false, |hub| {
        hub.publish(trouble("stall"));
        std::thread::sleep(Duration::from_millis(800));
        hub.publish(trouble("stall"));
        std::thread::sleep(Duration::from_millis(800));
        finish(hub);
    });
    let (code, said) = h.join().expect("the wait thread");
    assert_eq!(code, 0, "a stall must not end the wait: {said}");
    assert!(said.contains("all done"), "{said}");
    assert!(began.elapsed() >= Duration::from_millis(1500));

    // The same stall with the flag: exit 40 at once, naming the stall.
    let h = waiting(&wt, &hub, true, |hub| {
        hub.publish(trouble("stall"));
    });
    let (code, said) = h.join().expect("the wait thread");
    assert_eq!(code, 40, "--exit-on-stall exits on a stall: {said}");
    assert!(said.contains("\"stall\""), "{said}");

    // A quota refusal after a stall: 40 at once, and it is the quota that is reported.
    let h = waiting(&wt, &hub, false, |hub| {
        hub.publish(trouble("stall"));
        std::thread::sleep(Duration::from_millis(500));
        hub.publish(trouble("quota"));
    });
    let (code, said) = h.join().expect("the wait thread");
    assert_eq!(code, 40, "a quota refusal ends the wait: {said}");
    assert!(said.contains("\"quota\""), "{said}");

    // A timeout (a stall the transport gave up on) ends it too.
    let h = waiting(&wt, &hub, false, |hub| {
        hub.publish(trouble("timeout"));
    });
    let (code, said) = h.join().expect("the wait thread");
    assert_eq!(code, 40, "{said}");
    assert!(said.contains("\"timeout\""), "{said}");
}
