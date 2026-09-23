//! **The operator's own call door** — R24 part two, decisions 3 and 4.
//!
//! Two properties, and the first is the one the requirement names as *the* test:
//!
//! 1. **A head asking for `bash` through this door is refused BY THE DAEMON, in a sentence.**
//!    A head that can be talked into sending a name is not a hole while the daemon holds the
//!    list; a daemon that trusted the name it was sent is. So the refusal is asserted on the
//!    wire — a `Rejected` carrying why, not a dropped frame, because a head that cannot say
//!    why is a head that retries.
//! 2. **The two frames are a pair and the second is what writes the row.** The admission is
//!    recorded first (so `asked: true` is true), and a result for a call nobody admitted is
//!    refused rather than appended — a row with no admission behind it is a row nothing can
//!    be checked against.
//!
//! The failure case the pair exists for — a head that dies between the two frames — is
//! asserted in `hub`'s own tests, where the pending set and `detach` live.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, HEAD_RUN_TOOLS, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-oprun-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn start(tag: &str) -> (Arc<Registry>, ServerHandle) {
    let r = Registry::new();
    r.create("a", "", SessionWiring::default()).unwrap();
    let h = serve_registry(r.clone(), socket_path(tag)).expect("bind");
    (r, h)
}

/// Wait for the next frame that is not a `Delta` or an ack-shaped no-op.
fn next_frame(rx: &std::sync::mpsc::Receiver<Inbound>, ms: u64) -> Option<ServerFrame> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let f = inbound.frame();
        match f {
            // A resync is the hub moving the head, which this test does not provoke.
            ServerFrame::Resync { .. } => continue,
            other => return Some(other),
        }
    }
    None
}

/// **The negative test.** `bash` and `write` are refused by name, with a reason, and the
/// refusal names the list so the reader learns what the door is for rather than only that
/// they were wrong.
#[test]
fn a_head_asking_for_bash_through_the_operator_door_is_refused_by_the_daemon() {
    let (_r, handle) = start("refuse-bash");
    let (mut client, _hello, reader) = HeadClient::attach(
        handle.path(),
        "a",
        0,
        "tui",
        "dead",
        Caps::default(),
    )
    .expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    for name in ["bash", "write", "edit", "web_fetch --yolo"] {
        let _ = client
            .operator_call(0, &format!("c-{name}"), name, r#"{"command":"id"}"#)
            .expect("write");
        let f = next_frame(&rx, 3000).expect("the daemon must answer");
        match f {
            ServerFrame::Rejected { reason, .. } => {
                assert!(
                    reason.contains(name),
                    "the refusal must name what was asked for: {reason}"
                );
                assert!(
                    reason.contains(HEAD_RUN_TOOLS[0]) && reason.contains(HEAD_RUN_TOOLS[1]),
                    "and must name the list it accepts, or the reader cannot learn the door: \
                     {reason}"
                );
                assert!(
                    reason.contains("Nothing ran"),
                    "and must say nothing happened: {reason}"
                );
            }
            other => panic!("`{name}` was not refused by name: {other:?}"),
        }
    }
}

/// **The positive half: an allowed name is queued as the operator's own call, with the
/// identity the gate will record.**
///
/// **The admission itself is the worker's to write, so this test stops at the queue** — and
/// that is the honest boundary rather than a gap: the frame's job is to reach the queue with
/// the right fields, and the admission event's job belongs to the worker that drains it
/// (`harnessd`'s own tests). A test here that waited for `OperatorCallAllowed` would be
/// waiting for a worker this registry does not have, which is what the first draft of this
/// file did and what its failure message said.
#[test]
fn an_allowed_name_reaches_the_queue_as_the_operators_own_call() {
    let (r, handle) = start("allow");
    let hub = r.get("a").expect("session");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) = HeadClient::attach(
        handle.path(),
        "a",
        0,
        "tui",
        "dead",
        Caps::default(),
    )
    .expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    for (i, name) in HEAD_RUN_TOOLS.iter().enumerate() {
        let call_id = format!("c{i}");
        let _ = client
            .operator_call(0, &call_id, name, r#"{"url":"http://example.invalid"}"#)
            .expect("write");
        // The daemon answers this frame — a `Rejected` would be the refusal path, which the
        // negative test covers — so the assertion is that nothing refused it.
        // **Either answer proves it was not refused**: `Accepted` is the server's own ack,
        // and `CommandIssued` is the queue's, which arrives when the command is taken.
        let f = next_frame(&rx, 3000).expect("the daemon must answer");
        assert!(
            matches!(f, ServerFrame::Accepted { .. } | ServerFrame::Event(_)),
            "`{name}` must not be refused: {f:?}"
        );

        // And it arrived as the operator's own call, carrying WHO — the identity the corpus
        // records in `human:<who>` and the row carries in its `CallOrigin`.
        let cmd = hub.try_command().expect("the command is on the queue");
        match cmd.kind {
            letibot_sessionlog::hub::CommandKind::OperatorCall {
                call_id: got,
                name: got_name,
                who,
                ..
            } => {
                assert_eq!(got, call_id);
                assert_eq!(&got_name, name);
                assert_eq!(who, "dead", "the identity, not the head id");
            }
            other => panic!("`{name}` was not queued as an operator call: {other:?}"),
        }
    }
    let _ = head;
}

/// **The pending set does not leak, and `detach` is where it is cleared.**
///
/// The failure this covers is the pair's own: a head admitted for a call and gone before
/// reporting it. `Hub::note_operator_call` / `Hub::detach` are asserted directly here because
/// they are the mechanism, and the sentence they publish is what stops the corpus holding an
/// `admit` that nobody can account for.
#[test]
fn a_head_that_goes_away_between_the_frames_leaves_a_sentence_not_a_silent_admission() {
    use letibot_sessionlog::hub::Hub;

    let hub = Hub::new("a");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    hub.note_operator_call("c1", &head.head_id, "web_fetch", "dead");
    // While the head lives, nothing is said: the call is in flight, which is normal.
    assert!(
        !hub.retained().iter().any(|e| matches!(
            &e.event,
            SessionEvent::Warning { code, .. } if code == "operator_call_abandoned"
        )),
        "an in-flight call must not be reported as abandoned"
    );

    hub.detach(&head.head_id);
    let said = hub.retained().into_iter().find_map(|e| match e.event {
        SessionEvent::Warning { code, detail } if code == "operator_call_abandoned" => Some(detail),
        _ => None,
    });
    let detail = said.expect("the admission must be accounted for when the head is gone");
    assert!(
        detail.contains("web_fetch") && detail.contains("dead") && detail.contains("c1"),
        "the sentence must name the call, the name and the actor: {detail}"
    );
    assert!(
        detail.contains("the result is not"),
        "and must say what is missing rather than that something failed: {detail}"
    );
}

/// **R11's locator, end to end: a head asks by `(kind, id)` and gets bytes or *not recorded*.**
///
/// Three facts and they must be three: bytes that exist, a field that was recorded and is
/// empty, and a field nobody kept. The store holds `NULL` on every row written before R11 kept
/// the exchange, so *"nobody kept this"* and *"here it is, and it is empty"* are both real and
/// a head that could not tell them apart would draw one sentence over the other.
#[test]
fn the_diagnostic_locator_answers_bytes_empty_and_not_recorded_as_three_things() {
    use letibot_sessionlog::protocol::DiagnosticKind;
    use std::sync::Arc;

    /// A source with the three cases, since the wire test has no store.
    struct Three;
    impl letibot_sessionlog::registry::DiagnosticSource for Three {
        fn diagnostic(&self, request_id: &str, kind: DiagnosticKind) -> Option<String> {
            match (request_id, kind) {
                ("has-bytes", DiagnosticKind::Brief) => Some("brief — a file was read".into()),
                ("has-bytes", DiagnosticKind::Reply) => Some("ALLOW 0".into()),
                ("empty", DiagnosticKind::Brief) => Some(String::new()),
                _ => None,
            }
        }
    }

    let r = Registry::new();
    r.create("a", "", SessionWiring::default()).unwrap();
    r.set_diagnostic_source(Arc::new(Three));
    let h = serve_registry(r.clone(), socket_path("diag")).expect("bind");
    let (mut client, _hello, reader) =
        HeadClient::attach(h.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    let ask = |client: &mut HeadClient, id: &str, kind: DiagnosticKind| {
        client.fetch_diagnostic(id, kind).expect("write");
    };

    // 1. Bytes.
    ask(&mut client, "has-bytes", DiagnosticKind::Brief);
    match next_frame(&rx, 3000) {
        Some(ServerFrame::Diagnostic { body, total, .. }) => {
            assert_eq!(body.as_deref(), Some("brief — a file was read"));
            assert_eq!(total, body.as_ref().unwrap().len());
        }
        other => panic!("no bytes came back: {other:?}"),
    }

    // 2. Recorded, and zero bytes — `Some("")` and NOT `None`.
    ask(&mut client, "empty", DiagnosticKind::Brief);
    match next_frame(&rx, 3000) {
        Some(ServerFrame::Diagnostic { body, total, .. }) => {
            assert_eq!(
                body.as_deref(),
                Some(""),
                "a recorded-but-empty brief must not read as *not recorded*"
            );
            assert_eq!(total, 0);
        }
        other => panic!("{other:?}"),
    }

    // 3. Not recorded — `None`, which is a different fact from 2.
    ask(&mut client, "nobody-kept-this", DiagnosticKind::Reply);
    match next_frame(&rx, 3000) {
        Some(ServerFrame::Diagnostic { body, total, .. }) => {
            assert_eq!(body, None, "an unkept field must not read as empty");
            assert_eq!(total, 0);
        }
        other => panic!("{other:?}"),
    }

    // **The kind is carried, not inferred.** The same id asked for the other half is a
    // different answer, which is what makes `(kind, id)` a locator rather than an id.
    ask(&mut client, "has-bytes", DiagnosticKind::Reply);
    match next_frame(&rx, 3000) {
        Some(ServerFrame::Diagnostic { kind, body, .. }) => {
            assert_eq!(kind, DiagnosticKind::Reply);
            assert_eq!(body.as_deref(), Some("ALLOW 0"));
        }
        other => panic!("{other:?}"),
    }
}
