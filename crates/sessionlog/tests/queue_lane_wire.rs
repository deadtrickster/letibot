//! The queue lane as a head that is not this tree's own client sends it. rano writes the wire as
//! plain JSON (`rano/src/letibot.rs`), so the field is checked on the socket rather than through
//! `HeadClient`: `"delivery":"queue"` is the whole of what that client must add, and a daemon that
//! took the line as a steer would be the bug R290 fixed, one head over.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use serde_json::json;

const SESSION: &str = "s-queue-lane";

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-lane-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// How long one round trip may take: a local socket on a shared box is the box's fact.
const PATIENCE: Duration = Duration::from_secs(30);

/// A head attached to a real daemon, writing the frames as JSON.
struct Raw {
    out: UnixStream,
    lines: BufReader<UnixStream>,
}

impl Raw {
    fn attach(socket: &std::path::Path) -> Raw {
        let stream = UnixStream::connect(socket).expect("the daemon's socket");
        let lines = BufReader::new(stream.try_clone().expect("a second handle"));
        let mut raw = Raw { out: stream, lines };
        // rano's `Out::Attach`, field for field, with the protocol number that build speaks and the
        // three caps it has: `queue`, `can_decide`, `features`.
        raw.write(json!({
            "frame": "attach",
            "protocol_version": 72,
            "session_id": SESSION,
            "since_seq": 0,
            "kind": "tui",
            "identity": "rano",
            "caps": { "queue": 64, "can_decide": false, "features": ["queue_prompt_v1"] },
        }));
        match raw.until("hello") {
            ServerFrame::Hello { session_id, .. } => assert_eq!(session_id, SESSION),
            other => panic!("expected a hello, got {other:?}"),
        }
        raw
    }

    fn write(&mut self, frame: serde_json::Value) {
        let mut line = serde_json::to_string(&frame).expect("a frame serialises");
        line.push('\n');
        self.out
            .write_all(line.as_bytes())
            .expect("the head writes");
        self.out.flush().expect("the head flushes");
    }

    /// Every frame until one whose tag is `want`, which is what an answer to a request is.
    fn until(&mut self, want: &str) -> ServerFrame {
        let deadline = Instant::now() + PATIENCE;
        loop {
            assert!(
                Instant::now() < deadline,
                "waited {PATIENCE:?} for a `{want}` frame"
            );
            let mut line = String::new();
            let read = self.lines.read_line(&mut line).expect("the head reads");
            assert!(read > 0, "the daemon closed the connection");
            if line.trim().is_empty() {
                continue;
            }
            let said: serde_json::Value =
                serde_json::from_str(line.trim()).expect("the daemon writes a frame");
            if said["frame"] == want {
                return serde_json::from_value(said).expect("a frame this build knows");
            }
        }
    }

    /// One prompt, as rano's `Out::Prompt` spells it: the field is `delivery`, and it rides beside
    /// `text` and `context` rather than inside either.
    fn prompt(&mut self, id: &str, text: &str, lane: Option<&str>) {
        let mut frame = json!({
            "frame": "prompt",
            "client_request_id": id,
            "expected_seq": 0,
            "text": text,
            "attachments": [],
        });
        if let Some(lane) = lane {
            frame["delivery"] = json!(lane);
        }
        self.write(frame);
        match self.until("accepted") {
            ServerFrame::Accepted {
                client_request_id, ..
            } => assert_eq!(client_request_id, id),
            other => panic!("expected an accepted, got {other:?}"),
        }
    }
}

/// The lane the daemon filed the prompt under. The steering door is the one a RUNNING turn reads
/// and it takes only a steer, so a prompt it leaves in the queue is the worker's to run afterwards.
fn lane_taken(hub: &Hub) -> &'static str {
    use letibot_sessionlog::PromptDelivery;
    if hub.try_steering_command().is_some() {
        return "steer";
    }
    let took = hub.take_queued_prompts();
    match took.first().map(|c| &c.kind) {
        Some(CommandKind::Prompt {
            delivery: PromptDelivery::Queue,
            ..
        }) => "queue",
        Some(_) => "other",
        None => "gone",
    }
}

#[test]
fn a_client_that_sends_delivery_queue_gets_the_queue_lane() {
    let r = Registry::new();
    r.create(SESSION, "", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let hub = r.get(SESSION).expect("the session");
    let server = serve_registry(r.clone(), socket_path("queue")).expect("the socket binds");

    let mut head = Raw::attach(server.path());
    // The same words on each lane, so only the field can decide: a steer is what a running turn
    // takes at its next boundary, and a queued prompt is left for the worker.
    head.prompt("rano-1", "and then land it", Some("queue"));
    assert_eq!(
        lane_taken(&hub),
        "queue",
        "`delivery: \"queue\"` is the queue lane, and the running turn must not take it"
    );

    head.prompt("rano-2", "use the short form", None);
    assert_eq!(
        lane_taken(&hub),
        "steer",
        "a prompt with no `delivery` steers, as every prompt did before the field"
    );

    head.prompt("rano-3", "and this one too", Some("steer"));
    assert_eq!(
        lane_taken(&hub),
        "steer",
        "`steer` is the field's other word"
    );
}
