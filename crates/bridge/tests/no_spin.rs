//! **A daemon with nothing to say must not burn a core** — the measurement, not the argument.
//!
//! # The defect this exists for
//!
//! MEASURED 2026-10-26 on the operator's box: daemons whose sessions had been ended with the
//! bridge's `leticode_stop` sat at a full core each, per stopped session, until they were
//! killed. Seven of them, from another session's packages, with CPU time equal to their age —
//! one at 200%, for the two sessions it held. Nothing they were asked to do took that time: the
//! loop is `sessions::court_main`'s, one per session, and what span it was
//! `Sessions::dispatch` handing the `Interrupt` back to "the thread that owns the session" —
//! which, on that very thread, is itself. A command that goes back on the queue it was just
//! taken from is taken again in the same breath, for ever.
//!
//! **And this file lives in the bridge crate** because the shape it reproduces is the bridge's
//! own `leticode_stop`: it needs both halves — a bridge to stop a session with, and a daemon to
//! measure — and the bridge is the crate that has both.
//!
//! # The measurement, and why it is the whole process
//!
//! The daemon here is in this process (`Daemon::serve`, as `end_to_end.rs` builds it), so the
//! CPU the daemon burns is the CPU `getrusage(RUSAGE_SELF)` reports. That is exactly right for
//! the fact being asserted — a spinning session thread spends a core whoever's process it is
//! in — and it is measured over WINDOW of wall clock, so the assertion is a rate rather than a
//! total that three seconds of somebody else's work could disguise. This binary holds ONE test,
//! because a second one running beside it would be burning the CPU this one is accounting for.
//!
//! Every phase is a shape the field produced: a daemon nobody has attached to, a session stopped
//! after a turn, two sessions with one of them stopped, a stop repeated, and a stop of a session
//! that never ran a turn at all.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::tools::Bridge;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use serde_json::json;

/// The brief the first session is started with — the marker is how the test knows the turn it
/// waited for is the one the model answered.
const BRIEF: &str = "brief-marker: a daemon with nothing to do";
const ANSWER: &str = "answer-marker: done";

/// Generous on purpose: every wait here is for something a correct build produces in
/// milliseconds, and the assertion is what happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(60);

/// **How long each phase is measured over.** Long enough that a full core shows up as seconds
/// rather than as a rounding error, short enough that the six windows here cost under twenty
/// seconds of the suite's wall clock.
const WINDOW: Duration = Duration::from_secs(3);

/// **What counts as idle.** A spinning session thread spends about WINDOW of CPU (a whole core);
/// a daemon that is actually waiting spends a few milliseconds — its accept loop polls every
/// 20 ms, which is 150 wake-ups over this window and no measurable share of a core. A tenth of
/// the window is the line between *asleep* and *working*, and it is far from both.
fn idle_budget() -> Duration {
    WINDOW / 10
}

/// A directory that removes itself — a test that leaks a store per run fills the disk, and the
/// run that finds out is not this one.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-nospin-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// The CPU clock
// ---------------------------------------------------------------------------

/// This process's CPU time so far, user and system together.
///
/// `getrusage` and not `ps -o time=`: the daemon is in this process, so the process's own
/// accounting is the direct answer, it costs one syscall, and it needs no free-descriptor
/// round trip through a subprocess on a box where that is exactly the resource the tree keeps
/// running out of.
fn cpu_time() -> Duration {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `getrusage` writes into a `rusage` this frame owns, and `RUSAGE_SELF` cannot
    // fail for a live process.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    assert_eq!(rc, 0, "getrusage(RUSAGE_SELF)");
    let secs = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    secs(ru.ru_utime) + secs(ru.ru_stime)
}

/// **How much CPU this process burns while nothing is asked of it** — WINDOW of wall clock, with
/// the rate asserted against [`idle_budget`].
fn assert_idle(what: &str) -> Duration {
    let before = cpu_time();
    std::thread::sleep(WINDOW);
    let spent = cpu_time().saturating_sub(before);
    assert!(
        spent < idle_budget(),
        "{what}: the process spent {spent:?} of CPU over a {WINDOW:?} window with nothing to do \
         (the budget is {:?}). That is a session thread looping rather than blocking: \
         `court_main` takes a command off its session's queue and dispatches it, so a dispatch \
         path that puts the command back on that same queue takes it again at once, for ever.",
        idle_budget()
    );
    spent
}

// ---------------------------------------------------------------------------
// The model: a loopback server answering the provider's own frames
// ---------------------------------------------------------------------------

/// A fake provider on a loopback port, saying [`ANSWER`] to everything.
struct Fake {
    url: String,
}

impl Fake {
    fn serve() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    return;
                };
                let _ = read_request(stream.try_clone().expect("clone the socket"));
                let _ = write_sse(stream);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
        }
    }
}

/// Drain one HTTP request, so the provider's own reader does not see a half-sent body.
fn read_request(mut s: TcpStream) -> String {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let mut head_end = None;
    let mut want = 0usize;
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
        if head_end.is_none()
            && let Some(at) = acc.windows(4).position(|w| w == b"\r\n\r\n")
        {
            head_end = Some(at);
            want = acc[..at]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
        }
        if let Some(at) = head_end
            && acc.len() - (at + 4) >= want
        {
            break;
        }
        if acc.len() > (1 << 22) {
            break;
        }
    }
    match head_end {
        Some(at) => String::from_utf8_lossy(&acc[at + 4..]).into_owned(),
        None => String::from_utf8_lossy(&acc).into_owned(),
    }
}

/// The provider's frames: one delta, a finish reason, the usage block, and `[DONE]`.
fn write_sse(mut stream: TcpStream) -> std::io::Result<()> {
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"content": ANSWER}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens": 50,"completion_tokens": 9}}),
    );
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// The daemon, and the worktree it serves
// ---------------------------------------------------------------------------

/// A git repository with one commit — the bridge resolves a daemon by the workspace's hash, and
/// `workspace_of` asks git for the toplevel.
fn a_repository(at: &Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(at)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "-q", "--initial-branch=main"]);
    run(&["config", "user.email", "bridge@example.invalid"]);
    run(&["config", "user.name", "bridge test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(at.join("README.md"), "the base\n").expect("write");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "the base commit"]);
}

/// The daemon, the fake model and the config they share.
struct World {
    dir: TempDir,
    worktree: PathBuf,
    socket: PathBuf,
    registry: Arc<Registry>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    fn start() -> World {
        let dir = TempDir::new("e2e");
        let root = dir.path();
        // **Nothing of the operator's may reach this session**: a key in their
        // `providers.toml` or in the environment would decide what answers, and a mode row
        // in their store would decide the approval point. Both are pointed at scratch.
        std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
        // SAFETY: this test binary runs one test, and the variables are set before the
        // daemon, any thread that reads config, or the bridge exists.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_DATA_HOME", root.join("data"));
            std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("GLM_API_KEY");
            std::env::remove_var("LETIBOT_MODE");
            std::env::remove_var("LETIBOT_SOCKET");
        }

        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        a_repository(&worktree);

        let fake = Fake::serve();
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/no_spin.rs — a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-test\"\nurl = \"{}\"\n",
                fake.url
            ),
        )
        .expect("providers.toml");

        let socket = root.join("daemon.sock");
        // The bridge finds a folder's daemon by the workspace's hash, so `$LETIBOT_SOCKET`
        // names this one — the same override the shell honours, and the only way a test can
        // be sure it is talking to the socket it started.
        unsafe { std::env::set_var("LETIBOT_SOCKET", &socket) };

        let mut cfg = Config::for_this_box(&worktree);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        cfg.allow_bash = true;
        // The byte vocabulary: a session whose turns go to a provider needs no GGUF, which
        // is what keeps llama.cpp — and every model file on this box — out of this test.
        cfg.vocab_gguf = None;
        cfg.provider = Some(ProviderConfig {
            name: "deepseek".into(),
            model: Some("deepseek-chat".into()),
            api_key: None,
            thinking: false,
        });
        cfg.web_search = None;
        cfg.permission = Vec::new();
        cfg.http_retries = 0;
        cfg.context_window = Some(32_768);
        cfg.store = Some(root.join("sessions.db"));
        cfg.socket = socket.clone();
        cfg.session_id = "nospin-daemon-session".into();

        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("a provider session opens on the byte vocabulary"),
        ));
        // The operator's per-project mode rows are not this test's business.
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

        let session = cfg.session_id.clone();
        let registry = Registry::new();
        registry
            .create(session.clone(), "", Sessions::wiring(&cfg))
            .expect("a fresh registry takes the session");
        // Bound HERE rather than on the worker thread: the bridge decides whether to start a
        // daemon by connecting, and a socket that appears a moment later would be a race
        // between the test's first `start` and its own fixture.
        let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
        let worker_registry = registry.clone();
        let worker = std::thread::spawn(move || {
            let mut sessions = Sessions::open_first(parts, cfg, worker_registry)
                .expect("the daemon opens its first session");
            daemon.run(&mut sessions, |_, _| {});
            daemon.shutdown();
        });

        World {
            dir,
            worktree,
            socket,
            registry,
            worker: Some(worker),
        }
    }

    /// **One session the field's way**: `leticode_start` with a brief, and the turn it runs
    /// waited for by its answer — so the session a phase stops has really turned.
    fn a_session(&self, bridge: &Bridge, worktree: &str) -> String {
        let started = bridge
            .start(
                worktree,
                BRIEF,
                Some("deepseek/deepseek-chat"),
                "writes-allowed",
            )
            .expect("the bridge starts a session");
        let session = started["session"]
            .as_str()
            .expect("a session id")
            .to_string();
        self.until(bridge, &session, "the brief's turn to end", |v| {
            v["state"].as_str() == Some("idle")
                && v["last_message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(ANSWER)
        });
        session
    }

    /// Poll `status` until it says what we are waiting for, or the patience runs out.
    fn until(
        &self,
        bridge: &Bridge,
        session: &str,
        what: &str,
        ready: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let last = bridge
                .status(session)
                .unwrap_or_else(|e| panic!("status failed while waiting for {what}: {e}"));
            if ready(&last) {
                return last;
            }
            assert!(
                Instant::now() < deadline,
                "waited {PATIENCE:?} for {what}; the session says {last}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // The registry's close is what ends `Daemon::run`, so this is the orderly stop and
        // not a kill: every head wakes with Closed and the last turn's rows are written.
        self.registry.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        let _ = &self.dir;
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// **Four shapes the field produced, and the CPU each of them burns.**
///
/// One test rather than four, because the measurement is the whole process's and two of these
/// running beside each other would be measuring one another.
#[test]
fn a_daemon_with_nothing_to_do_does_not_burn_a_core() {
    let world = World::start();
    assert!(
        letibot_sessionlog::client::HeadClient::attach(
            &world.socket,
            "nospin-daemon-session",
            0,
            "remote",
            "probe",
            Default::default(),
        )
        .is_ok(),
        "the daemon must be serving before the bridge is asked to start anything"
    );
    let bridge = Bridge::new();
    let worktree = world.worktree.display().to_string();

    // --- 1. a daemon nobody has attached to --------------------------------
    // **The first session, and the review queue's, are both courts already** — `open_first`
    // eagerly, and `ensure_gatekeeper` on the daemon's first idle pass — so this phase would
    // catch a spin that has nothing to do with a head at all.
    std::thread::sleep(Duration::from_millis(500));
    let cold = assert_idle("a fresh daemon with no head");
    println!("  no head: {cold:?} of CPU over {WINDOW:?}");

    // --- 2. a session stopped after a turn ---------------------------------
    // **The field's shape exactly**: `leticode_start`, a turn, `leticode_stop`.
    let first = world.a_session(&bridge, &worktree);
    bridge.stop(&first).expect("the bridge stops the session");
    // The head's detach reaches the daemon on its own connection's thread, so the phase is
    // measured from the moment the bridge has stopped rather than from the moment it said so.
    std::thread::sleep(Duration::from_millis(500));
    let stopped = assert_idle("a session stopped after a turn");
    println!("  stopped after a turn: {stopped:?} of CPU over {WINDOW:?}");

    // --- 3. two sessions, ONE of them stopped ------------------------------
    // The 200% observation: a daemon holding two sessions spun on two cores. The second
    // session is running while its head is attached, so what this window proves is that the
    // stopped one costs nothing beside it — and then, below, that both of them stopped cost
    // nothing either.
    let second = world.a_session(&bridge, &worktree);
    let one_of_two = assert_idle("one of two sessions stopped, the other held by a head");
    println!("  one of two stopped: {one_of_two:?} of CPU over {WINDOW:?}");
    bridge.stop(&second).expect("the second session stops too");
    std::thread::sleep(Duration::from_millis(500));
    let two_stopped = assert_idle("both sessions of the daemon stopped");
    println!("  both stopped: {two_stopped:?} of CPU over {WINDOW:?}");

    // --- 4. the same stop again -------------------------------------------
    // The bridge no longer holds the session, so this is the refusal — and a refused stop is
    // a path the field took too (a package stopping its own session twice, or a script
    // stopping one that had already gone).
    let again = bridge.stop(&first);
    assert!(again.is_err(), "the bridge no longer holds it: {again:?}");
    let twice = assert_idle("a second stop of the same session");
    println!("  a second stop: {twice:?} of CPU over {WINDOW:?}");

    // --- 5. a stop of a session that never ran a turn ----------------------
    // **The review queue's session**: the daemon opens it on its first idle pass and it stays
    // idle until a row arrives, which on this workspace is never. Stopping it is the shape a
    // bridge watching `s-gatekeeper` produces.
    let held = bridge
        .attach(&worktree, "gatekeeper", false)
        .expect("the review queue's session is a session a head can drive");
    let idle_session = held["session"].as_str().expect("a session id").to_string();
    bridge
        .stop(&idle_session)
        .expect("a session with nothing running is stopped like any other");
    std::thread::sleep(Duration::from_millis(500));
    let idle = assert_idle("a stop of a session that never ran a turn");
    println!("  a stop of an idle session: {idle:?} of CPU over {WINDOW:?}");

    // And the daemon as a whole, one last window, after everything above.
    let settled = assert_idle("the daemon once every head is gone");
    println!("  settled: {settled:?} of CPU over {WINDOW:?}");
}
