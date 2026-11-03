//! The one door a tool call is here for, through the real binary.
//!
//! The daemon serves a connection from inside its own process tree only the askpass question:
//! every operator door is refused by name. The helper below is the production consumer of that one
//! door, spawned the way a command is - as a child of the process that owns the socket - so a
//! refusal that also swallowed the password would fail here rather than on a sudo prompt.

use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use letibot_sessionlog::Registry;
use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::serve_registry;

/// How long the helper may take before it is a failure rather than a wait.
const PATIENCE: Duration = Duration::from_secs(60);

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-askpass-tree-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// The helper, as `sudo` runs it: its prompt on argv and the session's socket in its environment,
/// which is exactly what every command in an unconfined session carries.
#[test]
fn the_askpass_helper_still_gets_the_password_from_inside_a_tool_tree() {
    let socket = socket_path("inside");
    let registry = Registry::of(Hub::new("s"));
    let server = serve_registry(registry, &socket).expect("bind");

    // The head a person is looking at, so the card has somewhere to be shown.
    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let (mut head, _hello, reader) =
        HeadClient::attach(&socket, "s", 0, "tui", "dead", caps).expect("head attach");
    let (tx, rx) = channel();
    let _pump = std::thread::spawn(move || pump(reader, tx));

    let child = std::process::Command::new(env!("CARGO_BIN_EXE_letibot-askpass"))
        .arg("[sudo] password for dead: ")
        .env("LETIBOT_SOCKET", server.path())
        .env("LETIBOT_SESSION", "s")
        .env("LETIBOT_COMMAND", "sudo apt install x")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the helper starts");
    let (out_tx, out_rx) = channel();
    let _waiter = std::thread::spawn(move || {
        let _ = out_tx.send(child.wait_with_output());
    });

    let deadline = Instant::now() + PATIENCE;
    let mut asked = false;
    let out = loop {
        while let Ok(inbound) = rx.recv_timeout(Duration::from_millis(20)) {
            if let ServerFrame::Event(envelope) = inbound.frame()
                && let SessionEvent::SecretRequested { req_id, .. } = envelope.event
            {
                head.secret(&req_id, Some("hunter2".into()))
                    .expect("the head answers the card");
                asked = true;
            }
        }
        if let Ok(result) = out_rx.try_recv() {
            break result.expect("the helper ends");
        }
        assert!(Instant::now() < deadline, "the helper never finished");
    };

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(asked, "the helper never raised a card: {said}");
    assert!(
        out.status.success(),
        "the helper was refused: {}\n{said}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hunter2\n",
        "the password is what the helper must print, and nothing else. {said}"
    );
}
