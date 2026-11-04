//! The bridge's half of a provider trouble: the typed event in `leticode_events` and
//! `leticode_status`, `wait` waking on it, and `leticode_model` moving a joined session.
//! One test, because `XDG_CONFIG_HOME` is process-global. Daemon side: `crates/harnessd/tests/provider_trouble.rs`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::tools::{Bridge, wait};
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::registry::Registry;
use serde_json::json;

/// The prompt each scenario sends.
const PROMPT: &str = "say hello";

/// The model the daemon's own session starts on.
const START: &str = "deepseek/deepseek-chat";

/// The model `leticode_model` moves it to.
const MOVED: &str = "glm/glm-4.6";

/// A directory that removes itself: a leaked store per run fills the disk.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-mcp-{tag}-{}-{n}", std::process::id()));
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

/// A fake provider on loopback: one answer per request, and the request bodies kept, because
/// *which model the turn went to* is the whole measurement.
struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn serve() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let body = read_request(stream.try_clone().expect("clone the socket"));
                record.lock().unwrap_or_else(|e| e.into_inner()).push(body);
                let _ = write_sse(stream);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            seen,
        }
    }

    /// The model each request named, in the order they arrived.
    fn models(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|b| {
                serde_json::from_str::<serde_json::Value>(b)
                    .ok()
                    .and_then(|v| v["model"].as_str().map(str::to_string))
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// Drain one HTTP request and hand back its body.
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

/// One answer, in the provider's own frame shape.
fn write_sse(mut stream: TcpStream) -> std::io::Result<()> {
    let mut body = format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{"content":"hello"}}]})
    );
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// The daemon, the fake provider and the config they share.
struct World {
    dir: TempDir,
    worktree: PathBuf,
    session: String,
    registry: Arc<Registry>,
    fake: Fake,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    fn start() -> World {
        let dir = TempDir::new("trouble");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
        // SAFETY: this binary runs one test, and the variables are set before the daemon, any
        // thread that reads config, or the bridge exists.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_DATA_HOME", root.join("data"));
            std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("GLM_API_KEY");
            std::env::remove_var("LETIBOT_MODE");
            // Never the gate's own demand: `LETIBOT_REQUIRE_APPARATUS` stays whatever the run
            // set, so a fixture here cannot turn this binary's skips green.
        }
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let fake = Fake::serve();
        // Two providers, one fake: the request that arrives is the same wire either way, and the model
        // named in its body is what says where the turn went.
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/provider_trouble.rs: a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n\n\
                 [glm]\nkey = \"sk-test\"\nurl = \"{url}\"\n",
                url = fake.url
            ),
        )
        .expect("providers.toml");

        let socket = root.join("daemon.sock");
        // SAFETY: as above, and before anything opens a socket.
        unsafe { std::env::set_var("LETIBOT_SOCKET", &socket) };

        let mut cfg = Config::for_this_box(&worktree);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        cfg.unconfined = true;
        cfg.allow_bash = false;
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
        cfg.session_id = "mcp-trouble-session".into();

        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("a provider session opens on the byte vocabulary"),
        ));
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

        let session = cfg.session_id.clone();
        let registry = Registry::new();
        registry
            .create(session.clone(), "", Sessions::wiring(&cfg))
            .expect("a fresh registry takes the session");
        let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
        // The record a joining bridge reads: without it the daemon is placed nowhere it can see.
        letibot_sessionlog::placement::write(&socket, "host").expect("the placement record");
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
            session,
            registry,
            fake,
            worker: Some(worker),
        }
    }

    fn hub(&self) -> Arc<letibot_sessionlog::hub::Hub> {
        self.registry.get(&self.session).expect("the session's hub")
    }

    /// The bridge's handle on the daemon's own session - the one another party started, which is the
    /// joined-daemon case.
    fn held(&self) -> Bridge {
        let bridge = Bridge::new();
        bridge
            .attach(&self.worktree.display().to_string(), &self.session, false)
            .expect("the bridge joins the running session");
        bridge
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.registry.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        let _ = &self.dir;
    }
}

/// Wait for a fact about the session, with a deadline so a regression reads as its missing fact.
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("waited 60s for {what}");
}

#[test]
fn a_provider_trouble_wakes_the_bridge_and_the_model_verb_moves_the_session() {
    let world = World::start();
    let bridge = world.held();
    let hub = world.hub();

    // A. The typed event as `leticode_events` shows it, and as `leticode_status` answers it: the
    // kind, the reset instant the provider gave, and the model the round was on.
    let reset = letibot_sessionlog::event::now_ms() + 90_000;
    hub.publish(SessionEvent::ProviderTrouble {
        kind: "rate_limit".into(),
        model: START.into(),
        detail: "http 429: rate limited".into(),
        reset_ms: Some(reset),
        count: 1,
        switched_to: None,
    });
    until("the trouble to reach the head", || {
        bridge
            .status(&world.session)
            .is_ok_and(|s| !s["last_trouble"].is_null())
    });
    let status = bridge.status(&world.session).expect("a status");
    assert_eq!(status["model"], START, "the model that answered: {status}");
    assert_eq!(status["last_trouble"]["kind"], "rate_limit", "{status}");
    assert_eq!(status["last_trouble"]["reset_ms"], reset, "{status}");
    assert_eq!(status["last_trouble"]["count"], 1, "{status}");
    // The ring says the same fact as one entry a caller can page through.
    let events = bridge.events(&world.session, None, None).expect("events");
    let entry = events["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|e| e["kind"] == "provider")
        .unwrap_or_else(|| panic!("no provider entry in {events}"))
        .clone();
    assert_eq!(entry["state"], "rate_limit", "{entry}");
    assert_eq!(entry["reset_ms"], reset, "{entry}");
    assert_eq!(entry["name"], START, "{entry}");

    // B. `letibot-mcp wait` returns on it, with the event on stdout, so an orchestrator wakes on a
    // stuck provider and not only on an ask or a failure.
    let worktree = world.worktree.display().to_string();
    let session = world.session.clone();
    let woken = std::thread::spawn(move || {
        wait(
            &worktree,
            &session,
            Instant::now() + Duration::from_secs(60),
        )
    });
    // After the wait has attached, or the event would be folded into its snapshot and never
    // wake it - which is the rule: a wait is answered by what it saw.
    std::thread::sleep(Duration::from_millis(400));
    hub.publish(SessionEvent::ProviderTrouble {
        kind: "quota".into(),
        model: START.into(),
        detail: "http 429: insufficient_quota".into(),
        reset_ms: None,
        count: 1,
        switched_to: None,
    });
    let (code, said) = woken.join().expect("the wait's thread");
    assert_eq!(
        code, 40,
        "the provider's trouble is its own exit code: {said}"
    );
    let payload: serde_json::Value = serde_json::from_str(&said).expect("the event, as JSON");
    assert_eq!(payload["kind"], "quota", "{said}");

    // C. `leticode_model` moves a RUNNING session that joined a daemon another session started:
    // the first turn goes to the model the daemon chose, the verb moves it, and the next turn's
    // request names the new one.
    bridge
        .send(&world.session, PROMPT)
        .expect("the first prompt is accepted");
    until("the first turn to reach the provider", || {
        !world.fake.models().is_empty()
    });
    assert_eq!(
        world.fake.models()[0],
        "deepseek-chat",
        "the first turn went to the model the session started on"
    );
    let moved = bridge
        .set_model(&world.session, MOVED)
        .expect("the switch is accepted");
    assert_eq!(moved["model"], MOVED, "{moved}");
    assert!(
        moved["said"]
            .as_str()
            .unwrap_or_default()
            .contains("glm-4.6"),
        "the daemon's own sentence names where it went: {moved}"
    );
    let before = world.fake.models().len();
    bridge
        .send(&world.session, PROMPT)
        .expect("the second prompt is accepted");
    until("the second turn to reach the provider", || {
        world.fake.models().len() > before
    });
    let models = world.fake.models();
    assert_eq!(
        models.last().map(String::as_str),
        Some("glm-4.6"),
        "the next turn's request names the model the verb set: {models:?}"
    );
    // And the status reads it back, so a caller that asks *where is this session* is answered from the
    // evidence rather than from what it last asked for.
    until("the status to name the new model", || {
        bridge
            .status(&world.session)
            .is_ok_and(|s| s["model"] == MOVED)
    });

    // D. A watched session refuses the verb. The head that holds the session is somebody else's,
    // and a bridge that moved it would be deciding for them; the refusal names the flag that would.
    let watcher = Bridge::new();
    watcher
        .attach(&world.worktree.display().to_string(), &world.session, true)
        .expect("a watching attach");
    let refused = watcher
        .set_model(&world.session, "deepseek/deepseek-chat")
        .expect_err("a watched session is not this bridge's to move");
    assert!(
        refused.contains("watch_only") && refused.contains("watched, not held"),
        "the refusal must name the reason and the flag: {refused}"
    );
}
