//! A command this daemon started is not a head.
//!
//! Every command a model's `bash` call runs is handed `LETIBOT_SOCKET` and `LETIBOT_SESSION`, so
//! `sudo`'s helper can put a password card in front of the operator. The socket's mode cannot tell
//! that process from a head - both are the operator's user - so the daemon asks the kernel which
//! process connected, and serves a connection from inside its own tree nothing but the askpass
//! door.
//!
//! The child here is a real process the daemon process spawned, which is the shape of a tool call.
//! The head is this test process, which is also the process `serve` runs in: a process is not
//! inside its own tree, and that is why it keeps every door.

use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::{ServerHandle, serve};
use letibot_sessionlog::wire::FrameReader;

/// How long one refused door may take to answer, in the child.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long the parent waits for the child. Longer than the child's own bounds, so a child that
/// stopped on a door that was not refused is reported with its own sentence rather than a timeout.
const CHILD_PATIENCE: Duration = Duration::from_secs(150);

/// Set in the child's environment, so the helper test knows it was re-executed as one.
const CHILD: &str = "LETIBOT_PEER_DOORS_CHILD";
/// The socket the child connects to.
const CHILD_SOCKET: &str = "LETIBOT_PEER_DOORS_SOCKET";

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-peer-doors-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn start(tag: &str) -> ServerHandle {
    let hub = Hub::new("s");
    serve(hub, socket_path(tag)).expect("bind")
}

/// Read frames until one is a refusal, and answer what the daemon said. Bounded, because a door
/// that was served instead of refused would otherwise wait for ever.
fn refusal(reader: &mut FrameReader<std::os::unix::net::UnixStream>) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no refusal arrived in {PATIENCE:?}"
        );
        let f: ServerFrame = reader.read().expect("a frame");
        if let ServerFrame::Rejected { reason, .. } = f {
            return reason;
        }
    }
}

/// Read frames until the answer to the password question, which is the one frame an askpass
/// connection exists for.
fn secret(reader: &mut FrameReader<std::os::unix::net::UnixStream>) -> (Option<String>, Option<String>) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no answer arrived in {PATIENCE:?}"
        );
        let f: ServerFrame = reader.read().expect("a frame");
        if let ServerFrame::Secret { secret, why } = f {
            return (secret, why);
        }
    }
}

/// The doors a head has and a tool call must not, in the operator's words: one entry per door,
/// which is what a person would try.
fn operator_doors() -> [&'static str; 11] {
    [
        "a ! line",
        "!term opening a pane",
        "!term keys",
        "!term closing a pane",
        "/mode",
        "a / verb",
        "the settings rows",
        "the session list",
        "typing a prompt to the model",
        "a decision",
        "stopping the daemon",
    ]
}

/// Send the frame that asks for one operator door. The list is `operator_doors`'s, in its order.
fn ask(client: &mut HeadClient, which: usize) {
    let r = match which {
        0 => client.operator_shell(0, "! sh").map(|_| ()),
        1 => client.term_open("!term sh", 80, 24),
        2 => client.term_input(0, b"id\n"),
        3 => client.term_close(0),
        4 => client.set_mode(0, "allow-all", true).map(|_| ()),
        5 => client.slash(0, "models").map(|_| ()),
        6 => client.settings(),
        7 => client.list_sessions(),
        8 => client.prompt(0, "type this as the operator").map(|_| ()),
        9 => client.answer("req-1", "allow_once").map(|_| ()),
        10 => client.stop(0, "somebody in the tool tree").map(|_| ()),
        other => panic!("no frame for door {other}"),
    };
    r.expect("the frame is written");
}

/// **The child.** Re-executed as this, it attaches like a head and then asks for every operator
/// door, printing what each answered. It is a process this daemon started, which is the whole of
/// the fact the daemon is meant to read.
#[test]
fn the_tool_trees_child() {
    if std::env::var(CHILD).as_deref() != Ok("1") {
        return;
    }
    let socket = std::env::var(CHILD_SOCKET).expect("the socket to connect to");
    let (mut client, _hello, mut reader) =
        HeadClient::attach(&socket, "s", 0, "tui", "tool-tree", Caps::default())
            .expect("a tool call's command can attach");

    for (which, door) in operator_doors().into_iter().enumerate() {
        ask(&mut client, which);
        println!("REFUSED {door} :: {}", refusal(&mut reader));
    }

    // And the one door this connection is here for: `sudo`'s password.
    client
        .askpass("[sudo] password for dead: ", "sudo apt install x")
        .expect("the ask is written");
    let (secret, why) = secret(&mut reader);
    println!("ASKED {secret:?} why={why:?}");
}

/// **The rule.** A connection from inside this daemon's tree is refused every operator door, by
/// name, and the password card still works.
#[test]
fn a_connection_from_inside_the_tool_tree_is_refused_every_operator_door() {
    let server = start("child");
    let socket = server.path().to_path_buf();

    // A head, so the password card has somebody to be shown to. It is the daemon's own process, and
    // a process is not inside its own tree.
    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let (mut head, _hello, head_reader) =
        HeadClient::attach(&socket, "s", 0, "tui", "dead", caps).expect("head attach");
    let (tx, rx) = channel();
    let _pump = std::thread::spawn(move || pump(head_reader, tx));

    let child = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "the_tool_trees_child", "--nocapture"])
        .env(CHILD, "1")
        .env(CHILD_SOCKET, &socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the child starts");
    let (out_tx, out_rx) = channel();
    let _waiter = std::thread::spawn(move || {
        let _ = out_tx.send(child.wait_with_output());
    });

    let deadline = Instant::now() + CHILD_PATIENCE;
    let mut answered = 0;
    let out = loop {
        while let Ok(inbound) = rx.recv_timeout(Duration::from_millis(20)) {
            if let ServerFrame::Event(envelope) = inbound.frame()
                && let SessionEvent::SecretRequested { req_id, .. } = envelope.event
            {
                head.secret(&req_id, Some("hunter2".into()))
                    .expect("the head answers the card");
                answered += 1;
            }
        }
        if let Ok(result) = out_rx.try_recv() {
            break result.expect("the child ends");
        }
        assert!(Instant::now() < deadline, "the child never finished");
    };
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "the child failed: {}\n{said}",
        String::from_utf8_lossy(&out.stderr)
    );

    for door in operator_doors() {
        let line = said
            .lines()
            .find(|l| l.starts_with(&format!("REFUSED {door} ::")))
            .unwrap_or_else(|| panic!("no refusal of {door} in:\n{said}"));
        assert!(
            line.contains("inside a session's tool tree"),
            "the refusal of {door} does not say why: {line}"
        );
    }
    assert!(
        said.contains("ASKED Some(\"hunter2\")"),
        "the password door was not served to the tool tree:\n{said}"
    );
    assert_eq!(
        answered, 1,
        "one card was raised for the tool tree's ask"
    );
}

/// **The other half.** A process that is not inside the daemon keeps every door: the `!` line runs
/// and the session list is answered.
#[test]
fn a_head_that_is_not_inside_the_tree_keeps_every_door() {
    let server = start("head");
    let socket = server.path().to_path_buf();
    let (mut client, _hello, mut reader) =
        HeadClient::attach(&socket, "s", 0, "tui", "dead", Caps::default()).expect("head attach");

    client
        .operator_shell(0, "! echo the operator ran this")
        .expect("the line is written");
    client.list_sessions().expect("the question is written");

    let deadline = Instant::now() + PATIENCE;
    let mut ran = false;
    let mut listed = false;
    while !(ran && listed) {
        assert!(
            Instant::now() < deadline,
            "the head was served neither door (ran = {ran}, listed = {listed})"
        );
        let f: ServerFrame = reader.read().expect("a frame");
        match f {
            ServerFrame::Accepted { .. } => ran = true,
            ServerFrame::Sessions { .. } => listed = true,
            // A refusal here would be the rule applied to a head, which is the bug.
            other @ ServerFrame::Rejected { .. } => {
                panic!("a head was refused a door it owns: {other:?}")
            }
            _ => {}
        }
    }
}
