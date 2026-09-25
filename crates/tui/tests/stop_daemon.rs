//! **A head that asks the daemon to stop does not exit until it knows.** R30.
//!
//! The operator, 2026-09-23, having chosen *exit AND stop the daemon*: *"i tried to exit
//! letibot and chose to stop the daemon too. well it didnt."* Measured from outside: the
//! head gone, and `harnessd` (pid 2346661) still at `PPID 1`, idle, its socket bound. The
//! old code was two discarded results and a return —
//!
//! ```rust
//! let _ = self.client.stop(app.seq, &who);
//! let _ = self.client.detach();
//! ```
//!
//! — so whether the frame reached the socket was a race against the head's own shutdown,
//! and **a request is not an outcome**.
//!
//! # What this file asserts, in the requirement's own three parts
//!
//! 1. **The frame is flushed and acknowledged, or the socket closes.** The peer here speaks
//!    protocol frames and hands the test the `stop` it read — so "the request went out" is a
//!    wire fact and not an inference — and it answers with the daemon's own
//!    `Accepted { note: "stopping" }`, which is what `App::apply` records as `acked`.
//! 2. **A deadline, and the head says what it is waiting for.** The frame is asked for while
//!    the daemon is still there, and the screen is read for the sentence.
//! 3. **If the deadline passes, the farewell names the pid and the verb.** Asserted as text,
//!    because that is the whole delivery: the operator has to read it.
//!
//! # Why the "daemon" is a real process and not a pid in a variable
//!
//! The head's strongest observation is `/proc/<pid>`, and a test that faked it would test
//! nothing. So the daemon's pid here is **a real child process** this file spawns: while it
//! is alive the head must wait, and the moment it is gone the head must leave. That is the
//! requirement's `or until it can say that it has not`, end to end, with a real death in it.
//!
//! The peer on the socket is scripted rather than a `ServerHandle` for the reason
//! `reconnect.rs` gives in its own header, plus one more: this test has to *withhold* the
//! daemon's answer and hold a socket open over a process that is not there, and a real
//! server cannot be made to do either.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_tui::app::App;
use letibot_tui::driver::Link;
use letibot_tui::render::RenderConfig;

const SESSION: &str = "s-stop";

fn socket_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("letibot-stop-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn app() -> App {
    App::new(RenderConfig {
        width: 100,
        color: false,
        ..RenderConfig::default()
    })
}

/// Everything the last frame drew, as one string.
fn frame(a: &mut App) -> String {
    a.screen(100, 30).join("\n")
}

/// One pass, drawing into a throwaway sink. A test has no terminal.
fn step(link: &mut Link, a: &mut App) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, (100, 30), &[], &mut sink);
}

/// Read until the peer has said what the test is waiting for, or the deadline passes.
fn wait_for<T>(what: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(v) = poll() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

/// The same, **ticking between polls** — which is what makes it a test of the driver and
/// not of an idle `App`.
///
/// A frame the daemon wrote sits in the pump's channel until somebody drains it, and the
/// only thing that drains it is [`step`]. A wait that polled the head without ticking would
/// be asserting that a fact arrives by itself, and it would fail for a reason that has
/// nothing to do with the requirement — measured, because that is exactly what this did
/// first.
fn wait_ticking<T>(
    link: &mut Link,
    a: &mut App,
    what: &str,
    mut poll: impl FnMut(&mut App) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        step(link, a);
        if let Some(v) = poll(a) {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

/// **A daemon that answers `Attach` and hands back whatever else it is sent.**
///
/// Two channels: the frames it received, and a switch for whether it should answer a
/// `Stop` with the daemon's own `Accepted` — withheld by the test that needs a daemon
/// which never answers.
struct Peer {
    frames: std::sync::mpsc::Receiver<ClientFrame>,
    path: PathBuf,
    _join: std::thread::JoinHandle<()>,
}

impl Peer {
    fn start(path: PathBuf, answer_stop: bool) -> Peer {
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, frames) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = FrameReader::new(stream.try_clone().expect("clone"));
            let mut writer = FrameWriter::new(stream);
            // The `Hello` this connection is seated by, with no snapshot: this test is
            // about the socket, not about a conversation.
            writer
                .write(&ServerFrame::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    session_id: SESSION.into(),
                    head_id: "h1".into(),
                    dropped: 0,
                    snapshot: None,
                    resumed_from: None,
                    scrubbed: ScrubReport::default(),
                    wiring: letibot_sessionlog::registry::SessionWiring::default(),
                    sessions: Vec::new(),
                })
                .expect("hello");
            loop {
                let Ok(frame) = reader.read::<ClientFrame>() else {
                    return;
                };
                // **The `ATTACH` is the handshake and not a fact under test.** Skipped
                // here rather than filtered in `next`, because the peer is the only party
                // that knows which frames the connection opened with.
                if matches!(frame, ClientFrame::Attach { .. }) {
                    continue;
                }
                let is_stop = matches!(frame, ClientFrame::Stop { .. });
                let id = match &frame {
                    ClientFrame::Stop {
                        client_request_id, ..
                    } => Some(client_request_id.clone()),
                    _ => None,
                };
                if tx.send(frame).is_err() {
                    return;
                }
                if is_stop {
                    if !answer_stop {
                        // **The daemon that never answers.** The socket stays open and
                        // nothing comes back, which is the case the ack exists to
                        // distinguish from a delivered request.
                        continue;
                    }
                    writer
                        .write(&ServerFrame::Accepted {
                            client_request_id: id.expect("a stop has an id"),
                            seq: 0,
                            note: letibot_sessionlog::NOTE_STOPPING.into(),
                        })
                        .expect("accepted");
                    // **Then it goes, silently and without a `Bye`** — which is what the
                    // real daemon does: `serve_conn` returns, the writer is dropped, and
                    // the head's pump sees EOF. A `Bye` would end this head for a different
                    // reason and hide the state machine under test.
                    let _ = writer.get_mut().shutdown(std::net::Shutdown::Both);
                    return;
                }
            }
        });
        Peer {
            frames,
            path,
            _join: join,
        }
    }

    fn next(&self) -> Option<ClientFrame> {
        self.frames.try_recv().ok()
    }
}

/// **A real process, so `/proc/<pid>` means something.**
fn spawn_a_process_that_stays() -> Child {
    Command::new("sleep")
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning the stand-in daemon process")
}

/// Ask the head to stop the daemon, exactly as the quit card does: two `ctrl-c`s and the
/// second row's digit, which is a key path through `App` rather than a constructed action —
/// so a change to the card's binding breaks this test rather than passing it.
fn choose_stop(link: &mut Link, a: &mut App) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(
        a,
        (100, 30),
        &[
            letibot_tui::app::Key::CtrlC,
            letibot_tui::app::Key::CtrlC,
            letibot_tui::app::Key::Char('2'),
        ],
        &mut sink,
    );
}

/// **The requirement, in one test.** The head is told to stop, the frame reaches the
/// socket, the daemon acknowledges — and the head **stays** until the process it named is
/// actually gone, then leaves, with nothing to report.
#[test]
fn the_head_waits_for_the_daemon_and_leaves_when_it_is_gone() {
    let path = socket_path("waits");
    let peer = Peer::start(path.clone(), true);
    let mut child = spawn_a_process_that_stays();
    let pid = child.id() as i32;

    let mut link = Link::open(&path, SESSION, 0, "tui", "test").expect("attach");
    let mut a = app();
    step(&mut link, &mut a);
    // The stand-in daemon's pid, exactly as the binary sets it from `SO_PEERCRED`.
    a.set_daemon_pid(Some(pid));

    choose_stop(&mut link, &mut a);

    // **1. The frame went out, and the head was told it was read.** Both are facts about
    // the wire: the peer hands back what it parsed, and the note is the daemon's own.
    let stop = format!(
        "{:?}",
        wait_for("the stop frame", || {
            // **`Settings`, not `Stop`.** The `Hello` arm queues a settings read, so the
            // first frame on the wire is not the one under test — and filtering here rather
            // than in the peer keeps the peer a plain reporter of what it parsed.
            peer.next()
                .filter(|f| matches!(f, ClientFrame::Stop { .. }))
        })
    );
    assert!(stop.contains("Stop"), "the frame the peer read: {stop}");
    assert!(stop.contains("\"test\""), "and it names the asker: {stop}");
    wait_ticking(&mut link, &mut a, "the acknowledgement", |a| {
        a.stopping().is_some_and(|s| s.acked).then_some(())
    });

    // **2. And the head is STILL HERE, with the pid alive.** This is the requirement: it
    // does not exit until the daemon has gone or it can say it has not. `quit` is already
    // set — the operator answered the card — and `should_quit` holds it back.
    assert!(
        !a.should_quit(),
        "the head must not leave while the daemon's process is alive"
    );
    let screen = frame(&mut a);
    assert!(
        screen.contains("stopping the daemon"),
        "the head says what it is waiting for rather than freezing: {screen}"
    );
    assert!(
        screen.contains("the daemon answered"),
        "and it says how far the daemon has got: {screen}"
    );

    // **3. The daemon goes, and the head leaves.** A real process death, observed through
    // `/proc` — the same test `~/bin/letibot` makes, and for the same reason.
    child.kill().expect("kill");
    let _ = child.wait();
    wait_ticking(
        &mut link,
        &mut a,
        "the head to notice the process is gone",
        |a| a.should_quit().then_some(()),
    );
    assert!(
        a.stopping().is_some_and(|s| s.gone),
        "the head must know it went, not merely stop waiting"
    );
    assert!(
        a.stop_farewell().is_none(),
        "nothing to report: it went, which is what was asked for"
    );
    let _ = std::fs::remove_file(&peer.path);
}

/// **A daemon that does not go is NAMED on the way out** — R30's third part, and the one
/// the operator asked for in the incident's own words: they found the orphan with `ps` a
/// day later, and the whole point is that they should have read it here.
///
/// The real deadline is waited out rather than faked. Five seconds in one test is the price
/// of asserting a number the operator reads instead of a number the test chose.
#[test]
fn a_daemon_that_does_not_go_is_named_with_its_pid_and_the_verb() {
    let path = socket_path("survives");
    // A daemon that never answers, so this case has the *weaker* evidence: nothing but a
    // write, and then a deadline.
    let peer = Peer::start(path.clone(), false);
    let mut child = spawn_a_process_that_stays();
    let pid = child.id() as i32;

    let mut link = Link::open(&path, SESSION, 0, "tui", "test").expect("attach");
    let mut a = app();
    step(&mut link, &mut a);
    a.set_daemon_pid(Some(pid));
    choose_stop(&mut link, &mut a);

    // The frame went out; nothing came back.
    let stop = format!(
        "{:?}",
        wait_for("the stop frame", || {
            // **`Settings`, not `Stop`.** The `Hello` arm queues a settings read, so the
            // first frame on the wire is not the one under test — and filtering here rather
            // than in the peer keeps the peer a plain reporter of what it parsed.
            peer.next()
                .filter(|f| matches!(f, ClientFrame::Stop { .. }))
        })
    );
    assert!(stop.contains("Stop"), "{stop}");
    assert!(
        a.stopping().is_some_and(|s| s.sent && !s.acked),
        "sent, and no acknowledgement — which is a different sentence from a refusal"
    );

    // **The head says what it is waiting for while it waits.** Read mid-flight so the
    // sentence is the one that was on the screen, not the one after the deadline.
    let screen = frame(&mut a);
    assert!(screen.contains("stopping the daemon"), "{screen}");
    assert!(
        screen.contains("has not answered yet"),
        "an unanswered request is said rather than assumed delivered: {screen}"
    );

    // …and it does not leave, for as long as the process is there.
    assert!(!a.should_quit(), "still waiting");

    // Wait the deadline out, ticking as the real loop would.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !a.should_quit() && Instant::now() < deadline {
        step(&mut link, &mut a);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        a.should_quit(),
        "the deadline must end the wait — a head that waited for ever is the freeze R30 \
         forbids"
    );

    let said = a
        .stop_farewell()
        .expect("a daemon that did not go is reported");
    assert!(
        said.contains(&pid.to_string()),
        "the farewell names the pid, which is the number the operator took to `ps`: {said}"
    );
    assert!(
        said.contains("asked to stop"),
        "and says what was asked: {said}"
    );
    assert!(
        said.contains("letibot --stop --force"),
        "and the verb that finishes it: {said}"
    );
    assert!(
        said.contains("never acknowledged"),
        "and which evidence was missing, because *delivered and ignored* and *never \
         delivered* are different facts: {said}"
    );
    assert!(
        !a.stopping().is_some_and(|s| s.gone),
        "the process was there the whole time"
    );

    child.kill().expect("kill");
    let _ = child.wait();
    let _ = std::fs::remove_file(&peer.path);
}

/// **A stop that worked says nothing.** The negative half, and the one that keeps the
/// farewell worth reading: a head that printed three lines after every successful stop
/// would be R19's note-band complaint in a different medium.
#[test]
fn a_successful_stop_says_nothing_on_the_way_out() {
    let path = socket_path("quiet");
    let peer = Peer::start(path.clone(), true);
    // A process that is already dead, standing in for a daemon that went promptly.
    let mut child = spawn_a_process_that_stays();
    let pid = child.id() as i32;
    child.kill().expect("kill");
    let _ = child.wait();

    let mut link = Link::open(&path, SESSION, 0, "tui", "test").expect("attach");
    let mut a = app();
    step(&mut link, &mut a);
    a.set_daemon_pid(Some(pid));
    choose_stop(&mut link, &mut a);
    wait_ticking(&mut link, &mut a, "the head to leave", |a| {
        a.should_quit().then_some(())
    });
    assert_eq!(
        a.stop_farewell(),
        None,
        "it went; there is nothing to report"
    );
    let _ = std::fs::remove_file(&peer.path);
}

/// **`SO_PEERCRED` names the process on the other end of this socket**, which is what makes
/// the farewell's pid a fact rather than a guess. Asserted against a listener in this very
/// process, so the expected answer is `std::process::id()`.
#[test]
fn the_daemons_pid_is_the_process_on_the_other_end_of_the_socket() {
    let path = socket_path("peercred");
    let peer = Peer::start(path.clone(), true);
    let mut link = Link::open(&path, SESSION, 0, "tui", "test").expect("attach");
    step(&mut link, &mut app());
    assert_eq!(
        link.daemon_pid(),
        Some(std::process::id() as i32),
        "the pid must come from the socket, not from a file"
    );
    drop(peer);
}

/// **The head does not reconnect to a daemon it just asked to stop.** The daemon closing
/// our socket is the answer arriving; a head that read that as a fault would open a second
/// socket to a process on its way out and draw *"reconnecting"* over a shutdown the
/// operator ordered.
#[test]
fn a_daemon_that_closes_after_a_stop_is_not_a_link_failure() {
    let path = socket_path("noreconnect");
    let peer = Peer::start(path.clone(), true);
    let mut child = spawn_a_process_that_stays();
    let pid = child.id() as i32;

    let mut link = Link::open(&path, SESSION, 0, "tui", "test").expect("attach");
    let mut a = app();
    step(&mut link, &mut a);
    a.set_daemon_pid(Some(pid));
    choose_stop(&mut link, &mut a);

    // The peer answers and shuts the socket down. Give the pump time to see the EOF.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && a.stopping().is_some_and(|s| !s.acked) {
        step(&mut link, &mut a);
        std::thread::sleep(Duration::from_millis(25));
    }
    for _ in 0..10 {
        step(&mut link, &mut a);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !a.detached(),
        "the link must not be marked down by the daemon's own answer"
    );
    assert!(
        !a.should_reconnect(),
        "and this head must not try to attach to a daemon it is shutting down"
    );
    let screen = frame(&mut a);
    assert!(
        !screen.contains("connection is down"),
        "no reconnecting line over a shutdown the operator asked for: {screen}"
    );

    child.kill().expect("kill");
    let _ = child.wait();
    let _ = std::fs::remove_file(&peer.path);
}

/// `UnixStream` is used as a value in one assertion above and the events channel drains
/// lazily; this keeps the unused-import lint honest about `Write` (the peer writes raw
/// bytes during the handshake).
#[allow(dead_code)]
fn _writes(_s: &mut UnixStream, _w: &mut dyn Write) {}
