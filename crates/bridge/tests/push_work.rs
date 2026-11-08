//! A held session between turns with a job still open says so once, and the job's exit is its own
//! event: the two facts a turn end does not carry, and the ones an orchestrator had no news of.

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
use letibot_transcript::{Backgrounding, ToolOutcome, TranscriptItem};
use serde_json::Value;

/// Generous: every wait here is for something a correct build produces in a second or two.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long a channel stays watched for a message that must not come - two of the watcher's
/// own ticks, so a pass that would have repeated itself has had the chance.
const QUIET: Duration = Duration::from_secs(9);

const SESSION: &str = "s-push-work";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One turn that leaves a job running: the model proposes `bash`, it goes to the background, and
/// the turn ends. The daemon's activity is quiet, which is `idle` with a job still open.
fn turn_with_a_job(hub: &Hub, id: &str, text: &str, job: &str) {
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

fn running(id: &str) -> letibot_sessionlog::protocol::JobEntry {
    letibot_sessionlog::protocol::JobEntry {
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

/// The events pushed, waiting for `want` of them: fewer is a failure that says what came.
fn events(rx: &Receiver<Value>, want: usize) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    while out.len() < want {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(note) = rx.recv_timeout(left) else {
            break;
        };
        out.push(note);
    }
    assert_eq!(
        out.len(),
        want,
        "the channel said {} of {want} events: {out:?}",
        out.len()
    );
    out
}

fn kind(note: &Value) -> &str {
    note["params"]["meta"]["event"].as_str().unwrap_or("")
}

fn content(note: &Value) -> &str {
    note["params"]["content"].as_str().unwrap_or("")
}

#[test]
fn an_idle_session_with_an_open_job_is_one_event_and_the_jobs_exit_is_another() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-mcp-push-work-{}", std::process::id()));
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
    // The head asks the daemon for its job table, which is what lists a job: the registry holds it.
    registry.set_jobs(SESSION, vec![running("j82015")]);

    turn_with_a_job(&hub, "t1", "the long one is running", "j82015");

    // The turn end, and then the fact this session has work open with nobody watching it.
    let notes = events(&rx, 2);
    assert_eq!(kind(&notes[0]), "turn_ended", "{notes:?}");
    let idle = &notes[1];
    assert_eq!(kind(idle), "idle_with_work", "{notes:?}");
    assert_eq!(idle["params"]["meta"]["session"], SESSION, "{idle}");
    assert_eq!(idle["params"]["meta"]["jobs"][0], "j82015", "{idle}");
    assert_eq!(idle["params"]["meta"]["rows"], 0, "{idle}");
    assert!(content(idle).contains("j82015"), "{idle}");

    // The same work open again is the same fact, and a poll is not news.
    assert!(
        rx.recv_timeout(QUIET).is_err(),
        "the same open job was said twice"
    );

    registry.set_jobs(SESSION, Vec::new());
    hub.publish(SessionEvent::JobSettled {
        job: "j82015".into(),
        state: "exited 0".into(),
        produced: 4096,
        elapsed_ms: 41_000,
    });
    let notes = events(&rx, 1);
    let ended = &notes[0];
    assert_eq!(kind(ended), "job_ended", "{notes:?}");
    assert_eq!(ended["params"]["meta"]["job"], "j82015", "{ended}");
    assert_eq!(ended["params"]["meta"]["state"], "exited 0", "{ended}");
    assert_eq!(ended["params"]["meta"]["session"], SESSION, "{ended}");
    assert!(content(ended).contains("exited 0"), "{ended}");

    // One settlement is one message, and the job that just ended is not still open work.
    assert!(
        rx.recv_timeout(QUIET).is_err(),
        "the job's exit was said twice, or the work it left is news it did not"
    );
}
