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

use std::io::{BufRead, Write};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve, serve_registry, serve_registry_rooted};
use letibot_sessionlog::testing::FOREIGN_TREE;
use letibot_sessionlog::wire::FrameReader;

/// How long one refused door may take to answer, in the child.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long the parent waits for the child. Longer than the child's own bounds, so a child that
/// stopped on a door that was not refused is reported with its own sentence rather than a timeout.
const CHILD_PATIENCE: Duration = Duration::from_secs(150);

/// Set in the child's environment, so the helper test knows it was re-executed as one.
const CHILD: &str = "LETIBOT_PEER_DOORS_CHILD";
/// Set in the seat child's environment, the same way.
const SEAT_CHILD: &str = "LETIBOT_PEER_SEAT_CHILD";
/// The socket the child connects to.
/// Set in the launcher child's environment, the same way.
const LAUNCHER_CHILD: &str = "LETIBOT_PEER_LAUNCHER_CHILD";
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

/// The same, over a session that is attached to something: a seat's wiring can only be told from
/// an empty one when the session has one.
fn start_wired(tag: &str) -> ServerHandle {
    let registry = Registry::new();
    let wiring = SessionWiring {
        model: "qwen-3.8-flash-next".into(),
        dialect: "qwen3.8".into(),
        endpoint: "127.0.0.1:8080".into(),
        workspace: "/home/dead/Projects/letibot".into(),
    };
    registry
        .create("s", "a session", wiring)
        .expect("a session");
    serve_registry(registry, socket_path(tag)).expect("bind")
}

/// The next line the child printed under `prefix`: a test binary prints libtest's own banner to
/// the same stdout, so the first line is never the one that was asked for.
fn next_line(
    prefix: &str,
    lines: &mut std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
) -> String {
    loop {
        let line = lines
            .next()
            .expect("a line from the child")
            .expect("readable stdout");
        if line.starts_with(prefix) {
            return line;
        }
    }
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
fn secret(
    reader: &mut FrameReader<std::os::unix::net::UnixStream>,
) -> (Option<String>, Option<String>) {
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

/// **The child.** Re-executed as this, it attaches asking for a head's caps and prints what the
/// seat it was given holds, then blocks until the parent says a head has arrived.
#[test]
fn the_seat_a_tool_call_is_given() {
    if std::env::var(SEAT_CHILD).as_deref() != Ok("1") {
        return;
    }
    let socket = std::env::var(CHILD_SOCKET).expect("the socket to connect to");
    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let (_client, hello, reader) =
        HeadClient::attach(&socket, "s", 0, "tui", "dead", caps).expect("attach");
    let ServerFrame::Hello {
        snapshot,
        sessions,
        wiring,
        ..
    } = hello
    else {
        panic!("the first frame was not a Hello: {hello:?}");
    };
    println!(
        "SEAT snapshot={} sessions={} wiring_empty={}",
        snapshot.is_some(),
        sessions.len(),
        wiring.workspace.is_empty() && wiring.model.is_empty()
    );
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .expect("a line from the parent");
    // A second is a thousand times what a pump needs to write a frame it already holds, so the
    // bound says "nothing arrived" rather than "nothing had arrived yet".
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let _ = tx.send(reader.read::<ServerFrame>().is_ok());
    });
    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(true) => println!("EVENT arrived"),
        Ok(false) => println!("EVENT unreadable"),
        Err(_) => println!("EVENT none"),
    }
}

/// **The seat.** A connection from a tool call is given none of a head's estate - no snapshot, no
/// session list, no wiring, no vote and no events - because the askpass door needs none of them.
#[test]
fn a_tool_calls_seat_holds_none_of_a_heads_estate() {
    let server = start_wired("seat");
    let socket = server.path().to_path_buf();

    let mut child = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "the_seat_a_tool_call_is_given", "--nocapture"])
        .env(SEAT_CHILD, "1")
        .env(CHILD_SOCKET, &socket)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the child starts");
    let mut to_child = child.stdin.take().expect("the child's stdin");
    let mut said = std::io::BufReader::new(child.stdout.take().expect("its stdout")).lines();

    let seat = next_line("SEAT ", &mut said);
    assert_eq!(
        seat, "SEAT snapshot=false sessions=0 wiring_empty=true",
        "a tool call's seat carried a head's estate: {seat}"
    );
    let hub = server.registry().resolve("s").expect("the session's hub");
    assert!(
        hub.deciding_heads().is_empty(),
        "a tool call is not a head that can decide: {:?}",
        hub.deciding_heads()
    );

    // A real head arriving is an event fanned to every seat, so it is what the child waits for.
    let _second = HeadClient::attach(&socket, "s", 0, "tui", "second", Caps::default())
        .expect("a second head attaches");
    to_child
        .write_all(b"go\n")
        .expect("the child is told to look");
    let event = next_line("EVENT ", &mut said);
    assert_eq!(
        event, "EVENT none",
        "a tool call was subscribed to the session's events: {event}"
    );

    let out = child.wait_with_output().expect("the child ends");
    assert!(
        out.status.success(),
        "the child failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    assert_eq!(answered, 1, "one card was raised for the tool tree's ask");
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

/// The launcher child: attaches and asks for the session list, an operator door.
#[test]
fn the_launcher_child() {
    if std::env::var(LAUNCHER_CHILD).as_deref() != Ok("1") {
        return;
    }
    let socket = std::env::var(CHILD_SOCKET).expect("the socket to connect to");
    let (mut client, _hello, mut reader) =
        HeadClient::attach(&socket, "s", 0, "tui", "launcher", Caps::default())
            .expect("the launcher attaches");
    client.list_sessions().expect("the question is written");
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "no answer in {PATIENCE:?}");
        match reader.read::<ServerFrame>().expect("a frame") {
            ServerFrame::Sessions { .. } => {
                println!("LISTED");
                return;
            }
            other @ ServerFrame::Rejected { .. } => println!("REFUSED :: {other:?}"),
            _ => {}
        }
    }
}

/// **A daemon rooted elsewhere.** An in-process server that names a tree root the child does not
/// descend from serves that child every door, and the default `serve_registry` (the other tests)
/// still refuses a descendant.
#[test]
fn a_child_of_a_daemon_rooted_elsewhere_keeps_every_door() {
    let registry = Registry::new();
    registry
        .create("s", "a session", SessionWiring::default())
        .expect("a session");
    let server =
        serve_registry_rooted(registry, socket_path("rooted"), FOREIGN_TREE).expect("bind");
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "the_launcher_child", "--nocapture"])
        .env(LAUNCHER_CHILD, "1")
        .env(CHILD_SOCKET, server.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the child runs");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the child failed: {said}");
    assert!(said.contains("LISTED"), "the child was refused: {said}");
    assert!(!said.contains("REFUSED"), "{said}");
}
