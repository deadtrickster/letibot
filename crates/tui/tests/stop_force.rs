//! **An exit-and-stop that the ordinary stop cannot finish is finished by the head itself, the
//! forced way — and the head says so.**
//!
//! The operator: *"yeah when i exited letidb and chose to stop daemon too - it couldnt. had to
//! --stop --force"*. The daemon's log showed an OLD build — `head "stop-force" (kind "remote")
//! attached speaking protocol 47, newer than this daemon's 45` — and asked whether the head
//! should detect an older daemon and take the force path itself, with a line saying why:
//! *"yes"*.
//!
//! # What this file asserts
//!
//! 1. **A same-version daemon that stops is not forced.** The ordinary stop, end to end, and no
//!    escalation and no sentence on the way out.
//! 2. **A daemon that hears the stop and does not go is forced once, automatically**, after
//!    the ordinary deadline: `the daemon did not stop within 15s — stopping it the forced way`,
//!    and its process ends.
//! 3. **An older daemon is forced directly**: no stop frame, no deadline wait, and the line
//!    `the daemon is an older build (protocol N, this head speaks M) — stopping it the forced
//!    way`.
//!
//! # Why the daemon is a real process, and what it is
//!
//! The force path signals a pid, and the head may signal only the process at the other end of
//! its socket — so the daemon here has to be **a separate process** that the head attaches to
//! and reads `SO_PEERCRED` from, exactly as the binary does. It is this test executable run
//! again with `--exact fixture_daemon` and an environment that says which daemon to be, and
//! what it runs is the daemon's own **`serve_registry`** over a real `Registry` — the server
//! `harnessd` runs — so the stop frame, the interrupt half's session discovery, the
//! `ListSessions` round trip and the goodbye are all the real ones. What the fixture changes is
//! only what each case is about:
//!
//! * `orderly` — exits once the registry is closed, as `harnessd`'s worker loop does;
//! * `wedged` — the real server takes the stop (ack, goodbye, registry closed) and the process
//!   then stays, ignoring SIGTERM: the measured 2026-10-06 shape of a daemon whose worker is
//!   inside something that will not end, and `harnessd`'s SIGTERM handler only closes a
//!   registry that is already closed;
//! * `older` — the real server behind a relay that rewrites the `Hello`'s protocol to an older
//!   one and **drops the stop frame**, as a build that cannot read it would. A head that asked
//!   it the ordinary way would wait out the whole deadline; one that forces directly does not.
//!
//! No `harnessd` binary: `CARGO_BIN_EXE_harnessd` is not visible from this crate, and a live
//! daemon needs a vocabulary or a provider this test has no business needing. Never a running
//! daemon of the operator's: every socket is in a scratch directory and every pid is a child
//! this test spawned.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_tui::app::{App, Key, STOP_DEADLINE_MS};
use letibot_tui::driver::Link;
use letibot_tui::ui::render::RenderConfig;

const SESSION: &str = "s-stop-force";
const FIXTURE_MODE: &str = "LETIBOT_TEST_STOP_FORCE_MODE";
const FIXTURE_SOCKET: &str = "LETIBOT_TEST_STOP_FORCE_SOCKET";
/// The protocol the `older` fixture claims: two behind this build, the operator's 45-vs-47.
const OLDER: u32 = PROTOCOL_VERSION - 2;

// ---------------------------------------------------------------------------
// The fixture daemon — this executable, run again
// ---------------------------------------------------------------------------

/// **The daemon process.** A no-op in an ordinary test run; the daemon when this executable is
/// started by [`spawn_daemon`] with the fixture's environment. It never returns in that case:
/// it exits, or it is killed.
#[test]
fn fixture_daemon() {
    let Ok(mode) = std::env::var(FIXTURE_MODE) else {
        return;
    };
    let socket = PathBuf::from(std::env::var(FIXTURE_SOCKET).expect("the fixture's socket"));
    let registry = Registry::new();
    registry
        .create(SESSION, "stop force", SessionWiring::default())
        .expect("a session to attach to");
    match mode.as_str() {
        "orderly" => {
            let server = serve_registry(registry.clone(), &socket).expect("bind");
            wait_closed(&registry);
            server.shutdown();
            std::process::exit(0);
        }
        "wedged" => {
            // SAFETY: setting a signal's disposition to ignore is async-signal-safe and touches
            // no Rust state.
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
            let _server = serve_registry(registry.clone(), &socket).expect("bind");
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        "older" => {
            let inner = socket.with_extension("inner");
            let server = serve_registry(registry.clone(), &inner).expect("bind");
            relay(&socket, inner);
            wait_closed(&registry);
            server.shutdown();
            std::process::exit(0);
        }
        other => panic!("no fixture mode {other}"),
    }
}

fn wait_closed(registry: &Registry) {
    while !registry.is_closed() {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// **An older build, as far as a head can tell**: every `Hello` names [`OLDER`], and a `Stop`
/// is never delivered. Everything else passes through to the real server untouched.
fn relay(outer: &Path, inner: PathBuf) {
    let listener = UnixListener::bind(outer).expect("bind the relay");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(head) = conn else { return };
            let inner = inner.clone();
            std::thread::spawn(move || {
                let Ok(daemon) = UnixStream::connect(&inner) else {
                    return;
                };
                let mut up_r = FrameReader::new(head.try_clone().expect("clone"));
                let mut up_w = FrameWriter::new(daemon.try_clone().expect("clone"));
                std::thread::spawn(move || {
                    while let Ok(f) = up_r.read::<ClientFrame>() {
                        if matches!(f, ClientFrame::Stop { .. }) {
                            continue;
                        }
                        if up_w.write(&f).is_err() {
                            break;
                        }
                    }
                    let _ = up_w.get_mut().shutdown(std::net::Shutdown::Both);
                });
                let mut down_r = FrameReader::new(daemon);
                let mut down_w = FrameWriter::new(head);
                while let Ok(mut f) = down_r.read::<ServerFrame>() {
                    if let ServerFrame::Hello {
                        protocol_version, ..
                    } = &mut f
                    {
                        *protocol_version = OLDER;
                    }
                    if down_w.write(&f).is_err() {
                        break;
                    }
                }
                let _ = down_w.get_mut().shutdown(std::net::Shutdown::Both);
            });
        }
    });
}

// ---------------------------------------------------------------------------
// The head's side
// ---------------------------------------------------------------------------

fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-stop-force-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Start the fixture daemon and wait for its socket to answer.
fn spawn_daemon(mode: &str, dir: &Path) -> (Child, PathBuf) {
    let socket = dir.join("harnessd.sock");
    let child = Command::new(std::env::current_exe().expect("this test executable"))
        .args([
            "--exact",
            "fixture_daemon",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FIXTURE_MODE, mode)
        .env(FIXTURE_SOCKET, &socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the fixture daemon starts");
    let deadline = Instant::now() + Duration::from_secs(20);
    while UnixStream::connect(&socket).is_err() {
        assert!(
            Instant::now() < deadline,
            "the fixture daemon never listened"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (child, socket)
}

fn app() -> App {
    App::new(RenderConfig {
        width: 100,
        color: false,
        ..RenderConfig::default()
    })
}

fn step(link: &mut Link, a: &mut App) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, (100, 30), &[], &[], &mut sink);
}

/// Attach exactly as the binary does: open, take the `Hello`, and seed the head's pid from
/// `SO_PEERCRED` on this very connection — the fixture's pid, read off the socket.
fn attach(socket: &Path) -> (Link, App) {
    let mut link = Link::open(socket, SESSION, 0, "tui", "test").expect("attach");
    let mut a = app();
    a.set_daemon_pid(link.daemon_pid());
    let deadline = Instant::now() + Duration::from_secs(10);
    while a.daemon_protocol().is_none() {
        assert!(Instant::now() < deadline, "no Hello");
        step(&mut link, &mut a);
        std::thread::sleep(Duration::from_millis(10));
    }
    (link, a)
}

/// The quit card's *exit, and stop the daemon too*: two ctrl-c's and the second row.
fn choose_stop(link: &mut Link, a: &mut App) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(
        a,
        (100, 30),
        &[Key::CtrlC, Key::CtrlC, Key::Char('2')],
        &[],
        &mut sink,
    );
}

/// Tick as the real loop does until the head would leave, reading the screen on the way;
/// returns every distinct screen it drew from the waiting line on, as one run of words.
fn run_to_quit(link: &mut Link, a: &mut App, budget: Duration) -> Vec<String> {
    let deadline = Instant::now() + budget;
    let mut seen: Vec<String> = Vec::new();
    while !a.should_quit() {
        assert!(
            Instant::now() < deadline,
            "the head never finished the stop: {:?}",
            a.stopping()
        );
        step(link, a);
        // The line wraps, so the screen is read as one run of words.
        let screen = a
            .screen(100, 30)
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if screen.contains("stopping the daemon") {
            let from = screen.find("stopping the daemon").unwrap();
            let l = screen[from..].to_string();
            if !seen.contains(&l) {
                seen.push(l);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    seen
}

fn is_gone(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(s) => s[s.rfind(')').unwrap() + 1..]
            .trim_start()
            .starts_with(['Z', 'X']),
    }
}

/// **(a) An ordinary stop on a same-version daemon is not escalated.** The real server takes
/// the stop, the process exits, and the head leaves with nothing forced and nothing to say.
#[test]
fn an_orderly_stop_is_not_forced() {
    let dir = scratch("orderly");
    let (mut child, socket) = spawn_daemon("orderly", &dir);
    let (mut link, mut a) = attach(&socket);
    assert_eq!(
        a.daemon_pid(),
        Some(child.id() as i32),
        "SO_PEERCRED names the fixture"
    );
    assert_eq!(a.daemon_protocol(), Some(PROTOCOL_VERSION));

    let started = Instant::now();
    choose_stop(&mut link, &mut a);
    run_to_quit(&mut link, &mut a, Duration::from_secs(30));

    let s = a.stopping().expect("a stop was asked for");
    assert!(s.sent && s.gone, "asked, and the process went: {s:?}");
    assert!(
        s.forced.is_none(),
        "a daemon that stopped is never forced: {s:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(STOP_DEADLINE_MS),
        "and it went well inside the deadline"
    );
    assert_eq!(a.stop_farewell(), None, "it went; nothing to report");
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// **(b) A daemon that hears the stop and does not go is forced once, and its process ends.**
///
/// The real deadline is waited out — fifteen seconds, then the force path's own five between
/// SIGTERM and SIGKILL — because the number is what the operator reads.
#[test]
fn a_daemon_that_does_not_go_is_forced_after_the_deadline() {
    let dir = scratch("wedged");
    let (mut child, socket) = spawn_daemon("wedged", &dir);
    let pid = child.id() as i32;
    let (mut link, mut a) = attach(&socket);
    assert_eq!(a.daemon_pid(), Some(pid));

    choose_stop(&mut link, &mut a);
    let lines = run_to_quit(&mut link, &mut a, Duration::from_secs(60));

    let secs = STOP_DEADLINE_MS / 1000;
    let escalated = format!("the daemon did not stop within {secs}s — stopping it the forced way");
    assert!(
        lines.iter().any(|l| l.contains(&escalated)),
        "the screen says it is forcing, and why, while it does: {lines:#?}"
    );
    let s = a.stopping().expect("a stop was asked for");
    assert!(s.sent, "the ordinary stop went out first: {s:?}");
    assert!(
        s.gone,
        "the forced way ended the process — the fixture ignores SIGTERM, so SIGKILL: {s:?}"
    );
    assert!(is_gone(pid), "and it really is gone");
    let said = a.stop_farewell().expect("a forced stop is said");
    assert!(said.contains(&escalated), "{said}");
    assert!(said.contains("SIGKILL ended it"), "what ended it: {said}");
    assert!(
        !said.contains("letibot --stop --force"),
        "it worked, so the operator is not sent to run the verb: {said}"
    );
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// **(c) An older daemon is forced directly, with the line saying why.** No stop frame (the
/// relay would have swallowed it, as a build that cannot read it does), no fifteen-second
/// wait, and the process ends on SIGTERM after the interrupt half has asked it about its turns.
#[test]
fn an_older_daemon_is_forced_directly() {
    let dir = scratch("older");
    let (mut child, socket) = spawn_daemon("older", &dir);
    let pid = child.id() as i32;
    let (mut link, mut a) = attach(&socket);
    assert_eq!(a.daemon_pid(), Some(pid));
    assert_eq!(a.daemon_protocol(), Some(OLDER), "the relay's Hello");

    let started = Instant::now();
    choose_stop(&mut link, &mut a);
    let lines = run_to_quit(&mut link, &mut a, Duration::from_secs(30));

    let why = format!(
        "the daemon is an older build (protocol {OLDER}, this head speaks {PROTOCOL_VERSION}) \
         — stopping it the forced way: in-flight turns are interrupted first"
    );
    assert!(
        lines.iter().any(|l| l.contains(&why)),
        "the line is on the screen while it waits: {lines:#?}"
    );
    let s = a.stopping().expect("a stop was asked for");
    assert!(!s.sent, "no stop frame to an older daemon: {s:?}");
    assert!(s.gone, "{s:?}");
    assert!(is_gone(pid));
    assert!(
        started.elapsed() < Duration::from_millis(STOP_DEADLINE_MS),
        "forced directly, not after the ordinary deadline"
    );
    let said = a.stop_farewell().expect("a forced stop is said");
    assert!(said.contains(&why), "and in the farewell: {said}");
    assert!(
        said.contains("no turns were in flight"),
        "the interrupt half reached the old daemon and asked: {said}"
    );
    assert!(said.contains("SIGTERM ended it"), "{said}");
    assert!(!said.contains("did not stop within"), "{said}");
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
