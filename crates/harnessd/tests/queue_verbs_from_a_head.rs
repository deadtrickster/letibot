//! The queue's verbs as a person runs them: a head sends `/queue restart ID` over the socket, the daemon parses it, scopes it to the session's repository and answers on the log. The model's half is the `queue` tool; the daemon's warnings tell the operator to type `/queue regate`, `restart`, `approve` and `rm`.
//!
//! The fixture is a real repository with a store, so each verb reaches the row lookup. The answer for a nonexistent row proves the verb ran rather than being dropped or unparsed.

use std::time::{Duration, Instant};

use letibot_harnessd::config::Config;
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::Registry;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-queue-verbs-{tag}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("a temp dir");
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A repository with one commit: `/queue` refuses a workspace that is in no repository before it
/// reads a row, so the verbs can only be measured against one that is.
fn a_repository(ws: &std::path::Path) {
    let git = |args: &[&str]| {
        let out = letibot_tools::testing::git(ws)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(ws.join("a.txt"), "one\n").expect("a file");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first"]);
}

#[test]
fn a_head_runs_the_queue_verbs_and_the_daemon_answers_them() {
    let dir = TempDir::new("verbs");
    let ws = dir.0.join("ws");
    std::fs::create_dir_all(&ws).expect("the workspace");
    a_repository(&ws);

    let mut cfg = Config::for_this_box(&ws);
    cfg.dialect = Dialect::Qwen;
    // The byte vocabulary: no turn runs here, so no GGUF is needed and no box lacks one.
    cfg.vocab_gguf = None;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.socket = dir.0.join("d.sock");
    cfg.session_id = "queue-verbs".into();
    let session = cfg.session_id.clone();
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    registry
        .create(session.clone(), "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let daemon = Daemon::serve(registry.clone(), &cfg.socket).expect("the socket binds");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    let (mut client, _hello, reader) =
        HeadClient::attach(&cfg.socket, &session, 0, "tui", "test", Caps::default())
            .expect("a head attaches");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump(reader, tx));

    // The test is the worker: take work off the bell until the command, and read the answer off
    // the log the way a head does.
    let mut send = |line: &str, sessions: &mut Sessions<'_>| -> String {
        client.slash(0, line).expect("the frame goes out");
        loop {
            match registry.next_work().expect("the bell is open") {
                letibot_sessionlog::registry::Work::Open(id) => {
                    let _ = sessions.open(&id);
                }
                letibot_sessionlog::registry::Work::Woken(_) => {}
                letibot_sessionlog::registry::Work::Close(_) => {}
                letibot_sessionlog::registry::Work::Command(id, cmd) => {
                    let _ = sessions.dispatch(&id, &cmd);
                    break;
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(frame) => {
                    if let letibot_sessionlog::client::Inbound::Frame(
                        letibot_sessionlog::protocol::ServerFrame::Event(env),
                    ) = frame
                        && let SessionEvent::Warning { code, detail, .. } = env.event
                        && code.starts_with("slash")
                    {
                        return detail;
                    }
                }
                Err(_) => panic!("no slash answer for `{line}` within 10s"),
            }
        }
    };

    // Every verb the daemon's own warnings name reaches the row lookup, which is the half a head
    // could not get to: each answer names the id it was given and says no row is called that.
    for line in [
        "queue restart nosuchrow",
        "queue approve nosuchrow",
        "queue rm nosuchrow",
    ] {
        let said = send(line, &mut sessions);
        assert!(
            said.contains("no row of either queue is called `nosuchrow`"),
            "`/{line}` did not reach the verb: {said}"
        );
    }

    // And `regate` reaches the gate's door rather than the row lookup: the answer names the target
    // it would gate, which only the resolver that read this workspace produces.
    let regated = send("queue regate", &mut sessions);
    assert!(
        regated.contains("main"),
        "`/queue regate` did not resolve this repository's target: {regated}"
    );

    daemon.shutdown();
}
