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
//!
//! **And because a scripted peer is a CLAIM about the daemon, one test here stands a real
//! `ServerHandle` up beside it** —
//! [`the_peer_sends_the_frames_a_real_daemon_sends_on_a_stop`] asks both the same question
//! and requires the same frames back. The claim was wrong once, in the mode that models
//! *what a real daemon does*: it closed silently with a comment asserting the daemon sends
//! no `Bye`, so the farewell this head prints on nearly every orderly stop had never been
//! exercised against the frame it actually gets.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_tui::app::App;
use letibot_tui::driver::Link;
use letibot_tui::ui::render::RenderConfig;

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
    link.tick(a, (100, 30), &[], &[], &mut sink);
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

/// **What the peer does when it is asked to stop.**
///
/// Three behaviours because there are three different questions to ask about a stop, and
/// the third is the one a real daemon actually produces — see [`OnStop::AcceptedThenBye`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnStop {
    /// Acknowledges, then closes the socket silently. The state machine (`acked`,
    /// `closed`, `gone`) is read through this one.
    Accepted,
    /// Says nothing at all, so the head has only a write and a deadline.
    Silent,
    /// **Acknowledges and then says goodbye, which is what a real daemon does.**
    ///
    /// `ServerHandle::shutdown` closes the registry, the seat's pump answers
    /// `Delivery::Closed` with `Bye { reason: "daemon shutting down" }`
    /// (`crates/sessionlog/src/server.rs`), and that is the last frame an orderly stop
    /// produces.
    ///
    /// **This mode is new, and its absence is why nothing caught the defect.** The peer
    /// used to close silently with a comment asserting that the real daemon sends no
    /// `Bye` — *"`serve_conn` returns, the writer is dropped, and the head's pump sees
    /// EOF"* — which is exactly backwards, and `reconnect.rs`'s header says so in as many
    /// words: *"the seat's pump answers `Delivery::Closed` with `ServerFrame::Bye { reason:
    /// \"daemon shutting down\" }` — and a `Bye` is by design not a drop."* Two test files
    /// disagreed about the daemon's behaviour, and the one that modelled a stop had it
    /// wrong, so the farewell below was never once exercised against the frame it
    /// actually gets.
    AcceptedThenBye,
}

/// **A daemon that answers `Attach` and hands back whatever else it is sent.**
///
/// Two channels: the frames it received, and how it should answer a `Stop`.
struct Peer {
    frames: std::sync::mpsc::Receiver<ClientFrame>,
    path: PathBuf,
    _join: std::thread::JoinHandle<()>,
}

impl Peer {
    /// The two-behaviour form: answer the stop, or never answer it.
    fn start(path: PathBuf, answer_stop: bool) -> Peer {
        let on_stop = if answer_stop {
            OnStop::Accepted
        } else {
            OnStop::Silent
        };
        Peer::start_with(path, on_stop)
    }

    fn start_with(path: PathBuf, on_stop: OnStop) -> Peer {
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
                    if on_stop == OnStop::Silent {
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
                    // **And, on the third mode, the goodbye a real daemon sends.** Written
                    // before the close, in the order the real one writes it: the ack, then
                    // the `Bye`, then the socket goes.
                    if on_stop == OnStop::AcceptedThenBye {
                        writer
                            .write(&ServerFrame::Bye {
                                reason: "daemon shutting down".into(),
                            })
                            .expect("bye");
                    }
                    // Then it goes. `Accepted` closes silently — the writer is dropped and
                    // the head's pump sees EOF — which is the case the state machine is
                    // read through.
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
        &[],
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

/// **The daemon's own goodbye is not the daemon being gone, and treating it as one is how
/// the head came to report a stop that had not happened.**
///
/// A real daemon shutting down does not close the socket silently: the seat's pump answers
/// `Delivery::Closed` with `Bye { reason: "daemon shutting down" }`, and that is the last
/// frame an orderly stop produces.
///
/// # The first correction, and what it over-corrected into
///
/// This test used to assert the opposite of what it asserts now, and its reasoning was half
/// right. The half that was right: the goodbye is written **before the process exits**, so
/// `Stopping::gone` — *observed gone, by reaping or by `/proc`* — cannot be true at the
/// instant it arrives. Read together with a farewell that required `gone`, that produced
/// this, in the operator's scrollback for days, on nearly every orderly stop:
///
/// ```text
/// letibot: the daemon was asked to stop and had not gone 0s later.
///   the request was acknowledged and did not stop; the daemon is still there (pid 2291248).
///   …
/// letibot: the daemon ended this head — daemon shutting down
/// ```
///
/// Two contradicting sentences four lines apart, and the advice is worse than the
/// contradiction: `--force` *aborts in-flight turns over the protocol*, and it was being
/// recommended against a daemon that had just left politely.
///
/// The half that was wrong is the conclusion. *A fact that cannot yet be observed* is not
/// *a fact that will not be*, and the answer to that is to keep waiting for the one
/// observation the head actually has — not to stop looking and call the frame the answer.
/// That is what this file asserted, and it is what produced the operator's **second**
/// report: *"it reports the server exited within a second — while `harnessd` is in fact
/// hung and has to be killed with `--force`."*
///
/// # Why the frame is not the answer, measured
///
/// `registry.close()` runs on the daemon's **connection thread**, the instant the `Stop` is
/// taken. The **worker** running the operator's command is a different thread, is inside
/// that command, and has not ended. On a live daemon, 2026-10-06: the `Bye` arrives **519 µs**
/// after the stop goes out, the daemon's process is still in `/proc` at that moment, and it
/// stays there for as long as the run holds the worker.
///
/// # What is asserted now, and the half that must not regress
///
/// The goodbye is still named by `farewell` (the reason outlives the screen), and the head
/// **does not leave on it** — it waits for the process. When the deadline passes with the
/// process still there, the sentence is the honest one: the daemon heard the request and
/// began shutting down, and it is still there. That is *not* the old contradicting pair,
/// because it is composed once, when the question is settled.
///
/// And the half the old test existed to defend is defended by the test below it: a stop
/// whose process really goes says **nothing at all**, goodbye or no goodbye.
#[test]
fn the_daemons_goodbye_is_not_the_daemon_being_gone() {
    let path = socket_path("goodbye");
    let peer = Peer::start_with(path.clone(), OnStop::AcceptedThenBye);
    // The peer runs in this test's own process, so the pid the head reads from
    // `SO_PEERCRED` is this process's — which never exits. That is the shape being
    // measured: a daemon that answered and did not go.
    let mut link = Link::open(&path, SESSION, 0, "tui", "test").expect("attach");
    let mut a = app();
    step(&mut link, &mut a);
    choose_stop(&mut link, &mut a);

    let stop = format!(
        "{:?}",
        wait_for("the stop frame", || peer
            .next()
            .filter(|f| matches!(f, ClientFrame::Stop { .. })))
    );
    assert!(
        stop.contains("Stop"),
        "the request reached the socket: {stop}"
    );
    wait_ticking(&mut link, &mut a, "the goodbye to arrive", |a| {
        a.farewell().is_some().then_some(())
    });

    assert!(
        a.stopping().is_some_and(|s| s.acked && !s.gone),
        "acknowledged, and the stronger observation was never available — which is the \
         whole reason a farewell keyed on `gone` cannot be right here"
    );
    assert_eq!(
        a.farewell(),
        Some("daemon shutting down"),
        "the goodbye is the fact the operator is left with"
    );
    assert!(
        !a.should_quit(),
        "**The correction.** The goodbye is published by the connection thread before the \
         worker has ended, so it is evidence that the request was READ and not that the \
         process has gone. Leaving here is how the head reported a stop that had not \
         happened — the operator then had to kill `harnessd` with `--force`."
    );

    // The deadline is the head's own way out, and the sentence it leaves says what it saw.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !a.should_quit() && Instant::now() < deadline {
        step(&mut link, &mut a);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(a.should_quit(), "the deadline must end the wait");
    let said = a
        .stop_farewell()
        .expect("the process is still there, so the head owes a sentence");
    assert!(
        said.contains("began shutting down"),
        "the daemon did hear the request, and that is the useful half: {said}"
    );
    assert!(
        said.contains("still there"),
        "and the sentence is about the PROCESS, which is the thing that has not gone: {said}"
    );
    assert!(
        said.contains("letibot --stop --force"),
        "and `--force` is offered as what it is, against a daemon that is genuinely still \
         there: {said}"
    );
    let _ = std::fs::remove_file(&peer.path);
}

/// **A stop whose process really went says nothing — goodbye or no goodbye.**
///
/// This is the half `the_daemons_goodbye_is_not_the_daemon_being_gone` used to defend, and
/// it must not regress: the operator's complaint that started it was *two contradicting
/// sentences on stderr four lines apart*. With the wait no longer cut short by the frame,
/// the sentence is composed only when the question is settled — and when the process is
/// gone, there is no sentence at all.
#[test]
fn a_stop_whose_process_really_went_says_nothing_even_with_a_goodbye() {
    let path = socket_path("goodbye-gone");
    let peer = Peer::start_with(path.clone(), OnStop::AcceptedThenBye);
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
    assert!(
        a.stopping().is_some_and(|s| s.gone),
        "the process really is gone, and that is the observation the head acts on"
    );
    assert_eq!(
        a.stop_farewell(),
        None,
        "it went; there is nothing to report, and a sentence here would be the two \
         contradicting lines the operator reported in the first place"
    );
    let _ = std::fs::remove_file(&peer.path);
}

/// **The scripted peer is pinned against a real daemon, so it cannot go on modelling
/// something the daemon does not do.**
///
/// The peer in this file is scripted on purpose — it has to withhold an answer and hold a
/// socket open over a process that is not there. But a scripted peer is a **claim about the
/// daemon**, and this one's `OnStop::Accepted` mode was written from a guess: it closed
/// silently with a comment asserting the real daemon sends no `Bye`. That is backwards, and
/// because it was the only model of an orderly stop in this file, the farewell the head
/// prints — *"the daemon ended this head — daemon shutting down"* — had **never once been
/// exercised against the frame it actually gets**, which is how the operator came to read
/// that line and *"the daemon was asked to stop and had not gone"* four lines apart on
/// nearly every stop.
///
/// So this test does not argue about the peer: it stands a real `ServerHandle` up
/// (`serve_registry`), asks it and the peer the same question over a socket, reduces each
/// connection to the frames a stop is made of, and requires them equal — and equal to the
/// two frames named below, so a failure says what a stop is rather than only that two things
/// disagree.
///
/// **One honest difference is dropped by the reduction.** A real daemon publishes a
/// `daemon_stopping` warning into every session before it acks, and a scripted peer has no
/// hub to publish into. That warning is a fact about the sessions and is asserted where it
/// belongs (`crates/sessionlog/tests/sessions.rs`); it is not what a stop *is* on the wire. A
/// stop is the ack, and then the farewell.
#[test]
fn the_peer_sends_the_frames_a_real_daemon_sends_on_a_stop() {
    // The real one, over a real socket, with a real registry behind it.
    let real_path = socket_path("realstop");
    let registry = Registry::new();
    registry
        .create(SESSION, "the stop", SessionWiring::default())
        .expect("a session to attach to");
    let server = serve_registry(registry.clone(), &real_path).expect("bind");
    let real = stop_frames(&real_path);
    server.shutdown();

    // The scripted one, asked exactly the same way.
    let peer_path = socket_path("peerstop");
    let peer = Peer::start_with(peer_path.clone(), OnStop::AcceptedThenBye);
    let scripted = stop_frames(&peer_path);

    let expected = vec![
        "accepted: stopping".to_string(),
        "bye: daemon shutting down".to_string(),
    ];
    assert_eq!(
        real, expected,
        "the real daemon's answer to a stop is the ack and the farewell, in that order"
    );
    assert_eq!(
        scripted, real,
        "the scripted peer has drifted from the daemon it stands in for. Every test in this \
         file that reads the head's stop state through it is reading a model, and this is the \
         one place that model is checked against the thing itself"
    );
    let _ = std::fs::remove_file(&peer.path);
}

/// **One stop, read off the socket, reduced to the frames a stop is made of.**
///
/// Attach, take the `Hello`, ask to stop, then read to the end of the connection. The read
/// has a deadline so a daemon that answers nothing ends the read instead of the test run — the
/// comparison above then fails with the short list, which is a better failure than a hang.
fn stop_frames(path: &std::path::Path) -> Vec<String> {
    let stream = UnixStream::connect(path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read deadline");
    let mut reader = FrameReader::new(stream.try_clone().expect("clone"));
    let mut writer = FrameWriter::new(stream);
    writer
        .write(&ClientFrame::Attach {
            protocol_version: PROTOCOL_VERSION,
            session_id: SESSION.into(),
            since_seq: 0,
            kind: "tui".into(),
            identity: "dead".into(),
            caps: Caps::default(),
        })
        .expect("attach");
    // The `Hello` first: a `Stop` from a connection that has not been seated is a different
    // question, and the real daemon is entitled to refuse it.
    match reader.read::<ServerFrame>() {
        Ok(ServerFrame::Hello { .. }) => {}
        other => panic!("expected the Hello, got {other:?}"),
    }
    writer
        .write(&ClientFrame::Stop {
            client_request_id: "r1".into(),
            expected_seq: 0,
            who: "dead".into(),
        })
        .expect("stop");

    let mut seen = Vec::new();
    while let Ok(f) = reader.read::<ServerFrame>() {
        if let Some(named) = stop_frame(&f) {
            seen.push(named);
        }
        if matches!(f, ServerFrame::Bye { .. }) {
            break;
        }
    }
    seen
}

/// **A frame of a stop, named** — `None` for everything a stop is not made of: the `Hello`,
/// a snapshot, the `daemon_stopping` event, a turn still finishing and reporting itself.
fn stop_frame(f: &ServerFrame) -> Option<String> {
    match f {
        ServerFrame::Accepted { note, .. } => Some(format!("accepted: {note}")),
        ServerFrame::Bye { reason } => Some(format!("bye: {reason}")),
        _ => None,
    }
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
