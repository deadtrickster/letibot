//! `ReadJobOutput` is answered off the queue while the session's host holds the job.
//!
//! The operator, with a turn four minutes into `Responding` and two background jobs running:
//! *"look at serendb window, try to get into a job - no output"*. The jobs list said one of
//! them had written 1.8 KB; entering it drew `reading…` and nothing else. The list is answered
//! on the connection thread off the registry (`ListJobs`); the window was submitted as a
//! command, and the command queue's one reader is the worker — the thread running the turn.
//! So the read waited for the turn to end.
//!
//! These tests hold the seam: no worker drains the queue here at all, which is exactly the
//! state a busy worker is in, and the window still arrives.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

fn start() -> Arc<Registry> {
    let registry = Registry::new();
    registry
        .create(
            "s-1",
            "one",
            SessionWiring {
                model: "qwen3-4b".into(),
                dialect: "qwen".into(),
                endpoint: "127.0.0.1:8080".into(),
                workspace: "/tmp/ws".into(),
            },
        )
        .expect("create");
    registry
}

fn attach(registry: &Arc<Registry>) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);
    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-1".into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "test".into(),
        caps: Caps::default(),
    })
    .expect("attach");
    assert!(matches!(
        r.read::<ServerFrame>().expect("hello"),
        ServerFrame::Hello { .. }
    ));
    (w, r)
}

fn running_window(job: &str) -> SessionEvent {
    SessionEvent::JobOutput {
        job: job.into(),
        from: 0,
        to: 1843,
        produced: 1843,
        dropped: 0,
        state: "running".into(),
        never_ran: false,
        lines: vec!["latest: Pulling from serenedb/serenedb-build-ubuntu".into()],
        next: None,
    }
}

/// **A running job's window arrives while nothing serves the queue.** The source is asked with
/// the job and offset the head sent, the event lands on the session's stream, and nothing is
/// queued behind the turn.
#[test]
fn a_running_jobs_output_is_answered_without_the_worker() {
    let registry = start();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    registry.set_job_output_source(
        "s-1",
        Arc::new(move |job, offset| {
            seen.lock().unwrap().push((job.to_string(), offset));
            Some(running_window(job))
        }),
    );
    let hub = registry.get("s-1").expect("the session");
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::ReadJobOutput {
        client_request_id: "head-1-r1".into(),
        job: "j16496".into(),
        offset: 0,
    })
    .expect("ask");
    let got = loop {
        match r.read::<ServerFrame>().expect("an answer") {
            ServerFrame::Event(env) => match env.event {
                e @ SessionEvent::JobOutput { .. } => break e,
                _ => continue,
            },
            other => panic!("expected the window on the stream, got {other:?}"),
        }
    };
    assert_eq!(got, running_window("j16496"));
    assert_eq!(*asked.lock().unwrap(), vec![("j16496".to_string(), 0)]);
    assert!(
        hub.try_command().is_none(),
        "a live read is not queued behind the turn"
    );
}

/// **A job the host does not hold is still the worker's**: its answer is the store's record of
/// how it ended. The source says `None` and the read is queued exactly as it always was.
#[test]
fn a_job_the_host_does_not_hold_is_queued_for_the_worker() {
    let registry = start();
    registry.set_job_output_source("s-1", Arc::new(|_, _| None));
    let hub = registry.get("s-1").expect("the session");
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::ReadJobOutput {
        client_request_id: "head-1-r1".into(),
        job: "j3".into(),
        offset: 7,
    })
    .expect("ask");
    loop {
        match r.read::<ServerFrame>().expect("an answer") {
            ServerFrame::Accepted { .. } => break,
            ServerFrame::Event(_) => continue,
            other => panic!("expected the queued read's Accepted, got {other:?}"),
        }
    }
    let cmd = hub.try_command().expect("queued for the worker");
    assert!(
        matches!(cmd.kind, CommandKind::ReadJobOutput { ref job, offset: 7 } if job == "j3"),
        "{:?}",
        cmd.kind
    );
}
