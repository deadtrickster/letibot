//! A `wait` holds on the session's own job and trusts the daemon's answer about a parked one,
//! over a real socket and `Registry`. A turn that ended with a job running must not answer
//! `the turn ended`; the daemon's `quiet` for a `wake: "later"` job must not be second-guessed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_bridge::head::{Attention, Head, START_GRACE};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::registry::SessionWiring;
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::testing;
use letibot_transcript::{Backgrounding, ToolOutcome, TranscriptItem};

/// Longer than the start grace by a margin: the point of the first half is that this wait is still
/// blocked after the grace, which a naive wait would answer inside.
const PAST_THE_GRACE: Duration = Duration::from_millis(START_GRACE.as_millis() as u64 + 1500);

/// A directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-mcp-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SESSION: &str = "s-wait-activity";

/// An assistant row, as the engine publishes one: the words a quiet wait answers with.
fn said(hub: &Hub, text: &str) {
    hub.publish(SessionEvent::TranscriptContent {
        item_id: format!("i-{text}"),
        item: Box::new(TranscriptItem::Assistant {
            text: text.to_string(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    });
}

/// A backgrounded call, which is what puts a handle in the head's own `jobs_open`.
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

fn started_a_job(hub: &Hub, handle: &str) {
    hub.publish(SessionEvent::ToolFinished {
        turn_id: "t1".into(),
        call_id: format!("c-{handle}"),
        outcome: ToolOutcome::Backgrounded {
            handle: handle.into(),
            ran_for_ms: 0,
            how: Backgrounding::Asked,
            next: format!("job_output {handle}"),
        },
        payload_digest: String::new(),
        inline_bytes: 0,
        full_bytes: 0,
        spill: None,
        repairs: 0,
        edit: None,
    });
}

#[test]
fn a_wait_holds_on_a_running_job_and_trusts_the_daemons_quiet() {
    let dir = TempDir::new("wait-activity");
    let root = dir.path().clone();
    std::fs::create_dir_all(root.join("config")).expect("config dir");
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
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
    let server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let hub = registry.get(SESSION).expect("the session's own hub");

    let head = Arc::new(
        Head::attach(
            &daemon::resolve(&worktree).expect("the daemon resolves"),
            SESSION,
        )
        .expect("the head attaches"),
    );
    // A turn ran and ended with words, so the wait has something to report as the last thing said,
    // and the head has seen a turn: the grace below applies to a session that has turned, and a
    // wait that cannot read a job's entry would sit it out and then poll.
    hub.publish(testing::turn_started("t1"));
    said(&hub, "the turn that started the job");
    hub.publish(testing::turn_finished("t1"));
    // And the daemon's own answer: this session holds a job. Nothing else is going to be
    // published for the length of the grace below, which is the window the wait must sit out.
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(vec![Waiting::Job("j7".into())]),
    });
    // Both of those are folded by the head's own thread, so the sample is taken once it holds them.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = head.snapshot();
        if seen.turned
            && seen
                .activity
                .waiting_on()
                .iter()
                .any(|w| *w == Waiting::Job("j7".into()))
            && seen.last_message() == "the turn that started the job"
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the head did not fold the turn and the job: {:?}",
            seen.activity
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The settlement and the turn it wakes, from another thread: the wait is what is being
    // measured, so nothing it is waiting for may be published by the thread calling it.
    let awakening = hub.clone();
    let settler = std::thread::spawn(move || {
        std::thread::sleep(PAST_THE_GRACE);
        said(&awakening, "the turn the settlement woke");
        awakening.publish(testing::turn_finished("t2"));
        awakening.publish(SessionEvent::ActivityChanged {
            activity: Activity::Quiet,
        });
    });

    let began = Instant::now();
    let answer = head.wait(Instant::now() + Duration::from_secs(30));
    let took = began.elapsed();
    settler.join().expect("the settler thread");
    match answer {
        Attention::TurnEnded { message } => {
            assert!(
                message.contains("the turn the settlement woke"),
                "the wait answered with the turn the job's settlement started, not the one before \
                 it (after {took:?}): {message}"
            );
            assert!(
                took >= PAST_THE_GRACE,
                "it held on the running job rather than answering inside the grace ({took:?})"
            );
        }
        other => panic!(
            "a session holding a job must not be answered inside its grace, and a quiet one must \
             be answered with the turn that ended: {other:?} after {took:?}"
        ),
    }

    // --- the parked half: the daemon's `quiet` is the answer, job or no job -----------------
    // The head's own `jobs_open` list still holds a job here, and the daemon says the session is quiet.
    // The head lists jobs from the daemon's table, which the registry holds.
    registry.set_jobs(SESSION, vec![running("j8")]);
    started_a_job(&hub, "j8");
    said(&hub, "the turn that parked the server");
    hub.publish(testing::turn_finished("t1"));
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::Quiet,
    });
    // The fold that fills `jobs_open` runs on the head's own thread, so the assertion waits for
    // it rather than reading the state the publish happened beside.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !head.snapshot().jobs_open.contains("j8") {
        assert!(
            Instant::now() < deadline,
            "the parked job is open, which is what makes this about the declaration"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let began = Instant::now();
    let answer = head.wait(Instant::now() + Duration::from_secs(20));
    let took = began.elapsed();
    match answer {
        Attention::TurnEnded { message } => assert!(
            message.contains("the turn that parked the server"),
            "a quiet session is answered with the last thing it said: {message}"
        ),
        other => panic!("a quiet session must end the wait, whatever is open: {other:?}"),
    }
    assert!(
        took < Duration::from_secs(10),
        "it did not sit out its deadline for a job nothing is waiting on ({took:?})"
    );
    drop(server);
}
