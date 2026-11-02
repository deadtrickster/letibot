//! A stop tells the head what it ended, and does so before that head is told `Bye`. A hub-level
//! test cannot see this - a closed hub still queues what is published to it, so the sentence is
//! there for anybody who asks - what comes apart is the ORDER the frames went out on the socket.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{PromptDriver, Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;

/// The operator's own run, as the `!send` door knows it: one run in flight and nothing else.
struct ARun;
impl PromptDriver for ARun {
    fn send(&self, _s: &str, _r: Option<&str>, _line: &str) -> Result<Option<String>, String> {
        Err("nothing is running in this fixture".into())
    }
    fn in_flight(&self) -> Option<(String, String)> {
        Some(("j1".into(), "/bin/cat".into()))
    }
}

fn socket(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Short: a socket path is capped at ~104 bytes and a Mac's temp dir is half of it.
    letibot_sessionlog::testing::scratch_root().join(format!(
        "lb-stop-{tag}-{}-{:x}.sock",
        std::process::id(),
        n as u32
    ))
}

/// The frames as they arrived, so the sentence's place among them is a fact.
fn frames_until_bye(rx: &std::sync::mpsc::Receiver<Inbound>) -> Vec<String> {
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        match inbound.frame() {
            ServerFrame::Event(env) => {
                if let SessionEvent::Warning { code, .. } = &env.event {
                    out.push(code.clone());
                }
            }
            ServerFrame::Bye { reason } => {
                out.push(format!("Bye: {reason}"));
                return out;
            }
            _ => {}
        }
    }
    out
}

#[test]
fn a_stop_names_the_run_it_ends_before_the_head_is_told_bye() {
    let r = Registry::new();
    r.create("stop", "", SessionWiring::default()).unwrap();
    r.set_prompt("stop", Arc::new(ARun));
    // The host's ender, taking a beat: the stand-in for `end_running` waiting on a cgroup.
    r.watch_runs(
        "stop",
        Arc::new(|| std::thread::sleep(Duration::from_millis(300))),
    );

    let path = socket("runs");
    let server = serve_registry(Arc::clone(&r), &path).expect("the socket binds");
    let (mut head, _hello, reader) =
        HeadClient::attach(&path, "stop", 0, "tui", "dead", Caps::default())
            .expect("the head attaches");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump(reader, tx));

    head.stop(0, "dead").expect("the stop goes out");
    let seen = frames_until_bye(&rx);
    server.shutdown();

    let sentence = seen.iter().position(|f| f == "daemon_stopping_runs");
    let bye = seen
        .iter()
        .position(|f| f.starts_with("Bye"))
        .unwrap_or(seen.len());
    assert!(
        sentence.is_some(),
        "the stop ended a run and never named it: {seen:?}"
    );
    assert!(
        sentence < Some(bye),
        "the sentence has to arrive before the `Bye`, or the head is told the daemon is going \
         while it still owes it the one fact about the run it ended: {seen:?}"
    );
}
