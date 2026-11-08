//! A package's job ends, and whether that is the SEAT's news: a session parked on the job is told
//! once, and a session that is working is not told at all - it reads its own job results.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use letibot_bridge::daemon;
use letibot_bridge::head::Head;
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::JobEntry;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::testing;
use letibot_transcript::{Backgrounding, ToolOutcome};
use serde_json::Value;

/// Generous: every wait here is for something a correct build produces in a second or two.
const PATIENCE: Duration = Duration::from_secs(30);

const SESSION: &str = "s-push-job-ends";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn running(id: &str) -> JobEntry {
    JobEntry {
        id: id.into(),
        command: "sleep 300".into(),
        slug: String::new(),
        how: "asked".into(),
        state: "running".into(),
        running: true,
        never_ran: false,
        redirect: None,
        produced: 0,
        elapsed_ms: 0,
    }
}

/// A turn that is still in flight, with a job it backgrounded and did not wait for.
fn a_turn_with_a_job_running(hub: &Hub, id: &str, job: &str) {
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(vec![Waiting::Turn]),
    });
    hub.publish(testing::turn_started(id));
    hub.publish(testing::proposed_on(id, "c1", "bash", "sleep 300"));
    hub.publish(SessionEvent::ToolFinished {
        turn_id: id.into(),
        call_id: "c1".into(),
        outcome: ToolOutcome::Backgrounded {
            handle: job.into(),
            ran_for_ms: 900,
            how: Backgrounding::Asked,
            next: format!("job_output {job}"),
        },
        payload_digest: "fnv1a:0000000000000000".into(),
        inline_bytes: 0,
        full_bytes: 0,
        spill: None,
        repairs: 0,
        edit: None,
    });
}

/// One note, or a failure saying what came instead.
fn a_note(rx: &Receiver<Value>) -> Value {
    match rx.recv_timeout(PATIENCE) {
        Ok(note) => note,
        Err(e) => panic!("the channel said nothing: {e}"),
    }
}

fn kind(note: &Value) -> &str {
    note["params"]["meta"]["event"].as_str().unwrap_or("")
}

#[test]
fn a_package_job_ending_under_a_running_turn_is_not_pushed() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-mcp-push-job-ends-{}", std::process::id()));
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
    letibot_bridge::push::watch(&head).attach(tx);
    registry.set_jobs(SESSION, vec![running("j82015")]);

    // The job ends while the turn that started it is still running: the package reads that result
    // itself, so it is not the seat's news.
    a_turn_with_a_job_running(&hub, "t1", "j82015");
    registry.set_jobs(SESSION, Vec::new());
    hub.publish(SessionEvent::JobSettled {
        job: "j82015".into(),
        state: "exited 0".into(),
        produced: 4096,
        elapsed_ms: 41_000,
    });

    // Then the turn ends, and the turn end is the FIRST thing the channel says - a `job_ended`
    // recorded before it would have arrived first.
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(Vec::new()),
    });
    hub.publish(testing::turn_finished("t1"));
    let note = a_note(&rx);
    assert_eq!(
        kind(&note),
        "turn_ended",
        "a package's job end is not pushed under a running turn: {note}"
    );
}
