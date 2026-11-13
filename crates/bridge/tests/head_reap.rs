//! A head whose daemon is over leaves the bridge: after a `Bye`, and after a socket that ends
//! without one. Its watcher records the end first, a client that comes back is handed it, and a
//! call naming the session is told it is not held, with its last state.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::head::WENT_AWAY;
use letibot_bridge::tools::Bridge;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::event::{Envelope, now_ms};
use letibot_sessionlog::protocol::{PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::view::{SessionView, ViewBounds};
use letibot_sessionlog::{ScrubReport, SessionEvent, SessionWiring, testing};
use letibot_transcript::TranscriptItem;
use serde_json::Value;

/// Generous: every wait here is for something a correct build produces in a few seconds, the
/// longest being a watcher resting out its tick before its last pass.
const PATIENCE: Duration = Duration::from_secs(60);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Conns = Arc<Mutex<HashMap<String, UnixStream>>>;

/// A daemon that answers each attach with a session that has finished one turn, then keeps the
/// connection for the test to end with a `Bye` or a bare hang-up.
fn fake_daemon(socket: &std::path::Path) -> Conns {
    let listener = UnixListener::bind(socket).expect("bind");
    let conns: Conns = Arc::default();
    let held = conns.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut line = String::new();
            // `is_up` connects and hangs up: an empty first line is that probe.
            if BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .unwrap_or(0)
                == 0
            {
                continue;
            }
            let attach: Value = serde_json::from_str(&line).expect("an attach frame");
            let session = attach["session_id"]
                .as_str()
                .expect("a session")
                .to_string();
            send(&mut stream, &hello(&session));
            for (seq, event) in one_turn().into_iter().enumerate() {
                send(
                    &mut stream,
                    &ServerFrame::Event(Envelope {
                        session_id: session.clone(),
                        seq: seq as u64 + 1,
                        ts: now_ms(),
                        event,
                    }),
                );
            }
            held.lock().unwrap().insert(session, stream);
        }
    });
    conns
}

fn send(stream: &mut UnixStream, frame: &ServerFrame) {
    writeln!(stream, "{}", serde_json::to_string(frame).unwrap()).expect("write a frame");
}

fn hello(session: &str) -> ServerFrame {
    ServerFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        session_id: session.into(),
        head_id: format!("h-{session}"),
        dropped: 0,
        snapshot: Some(Box::new(
            SessionView::new(session, ViewBounds::default()).snapshot(0, 0),
        )),
        resumed_from: None,
        scrubbed: ScrubReport::default(),
        wiring: SessionWiring::default(),
        sessions: Vec::new(),
    }
}

fn one_turn() -> Vec<SessionEvent> {
    vec![
        SessionEvent::ActivityChanged {
            activity: Activity::of(vec![Waiting::Turn]),
        },
        testing::turn_started("t1"),
        SessionEvent::TranscriptAppended {
            item_id: "i-t1".into(),
            kind: "assistant".into(),
            ledger_head: "beef".into(),
            item: Some(Box::new(TranscriptItem::Assistant {
                text: "last words".into(),
                tool_calls: Vec::new(),
                truncated: false,
            })),
            body_bytes: 0,
        },
        testing::turn_finished("t1"),
        SessionEvent::ActivityChanged {
            activity: Activity::of(Vec::new()),
        },
    ]
}

/// The next channel note for `session` with this event, skipping others.
fn note_for(rx: &Receiver<Value>, session: &str, event: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    let mut said = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "no `{event}` for {session} on the channel: {said:?}"
        );
        if let Ok(note) = rx.recv_timeout(left.min(Duration::from_secs(1))) {
            let meta = &note["params"]["meta"];
            if meta["session"] == session && meta["event"] == event {
                return note;
            }
            said.push(note);
        }
    }
}

fn held(bridge: &Bridge) -> Vec<String> {
    bridge.list()["sessions"]
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|s| s["session"].as_str().map(str::to_string))
        .collect()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(50));
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

/// Whether the peer of `stream` has let go: a read answers EOF.
fn peer_closed(stream: &UnixStream) -> bool {
    let mut s = stream.try_clone().expect("clone");
    s.set_read_timeout(Some(Duration::from_millis(50)))
        .expect("timeout");
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
}

#[test]
fn a_head_whose_daemon_is_over_leaves_the_bridge() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-head-reap-{}", std::process::id()));
    let _dir = TempDir(root.clone());
    for d in ["config", "data", "worktree", "run"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let socket = root.join("daemon.sock");
    // SAFETY: this binary runs one test, and these are set before the socket or a head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::remove_var("LETIBOT_STORE");
    }
    let conns = fake_daemon(&socket);
    let worktree = root.join("worktree").display().to_string();
    let bridge = Bridge::new();
    let (tx, rx) = mpsc::channel();
    bridge.push_sessions(tx);

    // A `Bye`: the head leaves the map, its socket is closed, and the end waits in the outbox.
    bridge.attach(&worktree, "s-bye", false).expect("attach");
    note_for(&rx, "s-bye", "turn_ended");
    drop(rx);
    let bye = conns
        .lock()
        .unwrap()
        .remove("s-bye")
        .expect("the connection");
    send(
        &mut bye.try_clone().unwrap(),
        &ServerFrame::Bye {
            reason: "daemon shutting down".into(),
        },
    );
    until("the head the daemon said Bye to is still held", || {
        !held(&bridge).contains(&"s-bye".to_string())
    });
    until("the head's socket was never closed", || peer_closed(&bye));
    let refused = bridge.status("s-bye").expect_err("not held any more");
    assert!(
        refused.contains("no session `s-bye` is held") && refused.contains("daemon shutting down"),
        "{refused}"
    );
    assert!(refused.contains("last words"), "{refused}");
    let (tx, rx) = mpsc::channel();
    bridge.push_sessions(tx);
    let failed = note_for(&rx, "s-bye", "failed");
    assert!(
        failed["params"]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("daemon shutting down"),
        "{failed}"
    );

    // EOF with no `Bye`: the head is over with its own reason, and its watcher ends rather than
    // repeating a turn end for a session that reads as quiet.
    bridge.attach(&worktree, "s-eof", false).expect("attach");
    note_for(&rx, "s-eof", "turn_ended");
    drop(rx);
    let eof = conns
        .lock()
        .unwrap()
        .remove("s-eof")
        .expect("the connection");
    eof.shutdown(std::net::Shutdown::Both).expect("hang up");
    drop(eof);
    until("the head whose socket ended is still held", || {
        !held(&bridge).contains(&"s-eof".to_string())
    });
    until("a push watcher outlived its daemon", || watchers() == 0);
    let refused = bridge.status("s-eof").expect_err("not held any more");
    assert!(
        refused.contains("no session `s-eof` is held") && refused.contains(WENT_AWAY),
        "{refused}"
    );
    let (tx, rx) = mpsc::channel();
    bridge.push_sessions(tx);
    let failed = note_for(&rx, "s-eof", "failed");
    assert!(
        failed["params"]["content"]
            .as_str()
            .unwrap_or_default()
            .contains(WENT_AWAY),
        "{failed}"
    );
}
