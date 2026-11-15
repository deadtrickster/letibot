//! A push watcher sleeps between passes whatever state its head is in: busy on something that is
//! not a turn, dropped by the bridge, or closed by the daemon. The instruments are the head's
//! `wakes` and the outbox's `passes`, counted over a window with nothing published.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_bridge::head::{Attention, Head, START_GRACE};
use letibot_bridge::push::{Rules, watch};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::testing;
use letibot_transcript::TranscriptItem;

const PATIENCE: Duration = Duration::from_secs(30);

/// The window passes and wakes are counted over.
const WINDOW: Duration = Duration::from_secs(2);

/// Far above what a sleeping watcher does in [`WINDOW`], far below what a polling one does.
const FEW: u64 = 10;

const BUSY: &str = "s-spin-busy";
const DROPPED: &str = "s-spin-dropped";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_watcher_sleeps_on_a_busy_head_and_ends_on_a_dropped_or_closed_one() {
    let root =
        testing::scratch_root().join(format!("letibot-mcp-push-spin-{}", std::process::id()));
    let _dir = TempDir(root.clone());
    for d in ["config", "data", "worktree", "run"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let socket = root.join("daemon.sock");
    // SAFETY: this binary runs one test, and these are set before the socket or the head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::remove_var("LETIBOT_STORE");
    }
    let registry = Registry::new();
    for s in [BUSY, DROPPED] {
        registry
            .create(s, "", SessionWiring::default())
            .expect("a fresh registry takes the session");
    }
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let resolved = daemon::resolve(&root.join("worktree")).expect("the daemon resolves");
    let attach = |s: &str| Arc::new(Head::attach(&resolved, s).expect("the head attaches"));

    // A session that has turned and now waits on a monitor: busy, but on nothing `wait` arms on.
    let busy = attach(BUSY);
    let hub = registry.get(BUSY).expect("the session's hub");
    hub.publish(testing::turn_started("t1"));
    hub.publish(SessionEvent::TranscriptContent {
        item_id: "i-1".into(),
        item: Box::new(TranscriptItem::Assistant {
            text: "armed a monitor".into(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    });
    hub.publish(testing::turn_finished("t1"));
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(vec![Waiting::Monitor("ci".into())]),
    });
    until("the head folds the turn and the monitor", || {
        let s = busy.snapshot();
        s.turned && s.activity.waiting_on() == [Waiting::Monitor("ci".into())]
    });

    // `wait` past its grace on that session: it blocks to the deadline, it does not poll.
    let before = busy.wakes();
    let began = Instant::now();
    let answer = busy.wait(began + START_GRACE + WINDOW);
    let woke = busy.wakes() - before;
    assert!(matches!(answer, Attention::Timeout), "{answer:?}");
    assert!(
        began.elapsed() >= START_GRACE + WINDOW,
        "the wait answered early"
    );
    assert!(
        woke <= FEW,
        "a wait on a busy session past its grace looked {woke} times"
    );

    // Its watcher, over a window that crosses the grace of the wait it is in.
    let rules = Arc::new(Rules::none());
    let busy_box = watch(&busy, rules.clone());
    let dropped = attach(DROPPED);
    let dropped_box = watch(&dropped, rules);
    until("both watchers start", || watchers() == 2);
    let (passes, wakes) = (busy_box.passes(), busy.wakes());
    std::thread::sleep(START_GRACE + WINDOW);
    let passes = busy_box.passes() - passes;
    let wakes = busy.wakes() - wakes;
    assert!(passes <= 2, "the watcher made {passes} passes");
    assert!(wakes <= FEW, "the watcher's waits looked {wakes} times");

    // A head the bridge let go of: its watcher returns after the pass it is in.
    drop(dropped);
    assert!(
        dropped_box.wait_ended(PATIENCE),
        "the watcher of a dropped head did not end"
    );
    until("the dropped head's watcher thread exits", || {
        watchers() == 1
    });

    // The daemon goes away with the busy head still held: the watcher ends, it does not spin.
    registry.close();
    assert!(
        busy_box.wait_ended(PATIENCE),
        "the watcher of a closed head did not end"
    );
    until("the closed head's watcher thread exits", || watchers() == 0);
    let passes = busy_box.passes();
    let wakes = busy.wakes();
    std::thread::sleep(WINDOW);
    assert_eq!(busy_box.passes(), passes, "passes after the end");
    assert_eq!(busy.wakes(), wakes, "wakes after the end");
    drop(busy);
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// This process's push watcher threads, by the name `push::watch` gives them (cut to 15 bytes).
fn watchers() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("procfs")
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .filter(|comm| comm.trim_end() == "leticode-push-s")
        .count()
}
