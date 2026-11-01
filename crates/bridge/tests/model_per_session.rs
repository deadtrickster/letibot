//! **Each session's model is its own**, over the wire, once.
//!
//! The report: `leticode_start {model: "deepseek"}` on a worktree whose daemon a `glm` session had
//! started answered with glm's own 429, because the daemon's `--provider` is the *default* and
//! nothing carried the caller's model into the session that was created for it. So this measures
//! the two halves of the fix:
//!
//! | what | how |
//! |---|---|
//! | a join with another model runs THAT session's turns on it | the provider's own request body names the model |
//! | and the choice is the session's, durably | the store's `provider_choice` row for that session, and the daemon's own session left alone |
//! | a model this box cannot serve is refused before the brief | the error, and the provider that was never asked |
//!
//! The daemon here runs on `glm` on purpose: it is the reported shape, and it is what makes a
//! session that inherits the daemon's provider distinguishable from one that named its own.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_bridge::tools::Bridge;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::Store;
use serde_json::json;

/// The brief, and the marker its answer carries.
const BRIEF: &str = "brief-marker: say the one line";
const ANSWER: &str = "answer-marker: said";
/// What the daemon's first session is on, which is the shape the report came from.
const DAEMON_MODEL: &str = "glm";

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

/// A fake provider on loopback: one sentence per request, and the request bodies kept, because
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
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let body = read_request(stream.try_clone().expect("clone the socket"));
                record.lock().unwrap().push(body);
                let _ = write_sse(stream);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            seen,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
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
        json!({"choices":[{"delta":{"content": ANSWER}}]})
    );
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[],"usage":{
            "prompt_tokens": 50,
            "completion_tokens": 9,
            "prompt_cache_hit_tokens": 20,
        }})
    ));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// The daemon, the fake provider and the config they share.
struct World {
    dir: TempDir,
    socket: PathBuf,
    store: PathBuf,
    session: String,
    registry: Arc<Registry>,
    fake: Fake,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    fn start() -> World {
        let dir = TempDir::new("model-per-session");
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
            std::env::remove_var("LETIBOT_SOCKET");
            std::env::remove_var("LETIBOT_REQUIRE_APPARATUS");
        }
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let fake = Fake::serve();
        // **Two providers, one fake**, so the request that arrives is the same wire either way
        // and the model named in its body is what says where the turn went.
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/model_per_session.rs: a fake on loopback.\n\
                 [glm]\nkey = \"sk-test\"\nurl = \"{url}\"\n\n\
                 [deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n",
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
        // **No process backend, so nothing of the apparatus is needed**: these turns call no tool,
        // and `unconfined` with no `bash` is the writable host view every mode check wants.
        cfg.unconfined = true;
        cfg.allow_bash = false;
        // The byte vocabulary: a session whose turns go to a provider needs no GGUF.
        cfg.vocab_gguf = None;
        cfg.provider = Some(ProviderConfig {
            name: DAEMON_MODEL.into(),
            model: None,
            api_key: None,
            thinking: false,
        });
        cfg.web_search = None;
        cfg.permission = Vec::new();
        cfg.http_retries = 0;
        cfg.context_window = Some(32_768);
        cfg.store = Some(root.join("sessions.db"));
        cfg.socket = socket.clone();
        cfg.session_id = "mcp-model-session".into();

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
        // **The record a joining bridge reads** (R102): without it the daemon is placed *nowhere it
        // can see*, and `leticode_start` refuses to join rather than guess where its tools run.
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
            socket,
            store: root.join("sessions.db"),
            session,
            registry,
            fake,
            worker: Some(worker),
        }
    }

    /// The session's own model, from the row the daemon writes when it switches.
    fn model_of(&self, session: &str) -> Option<String> {
        Store::open(&self.store)
            .expect("the store opens")
            .provider_choice(session)
            .expect("the session's row is readable")
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

#[test]
fn a_session_that_joins_a_daemon_runs_its_own_model() {
    let world = World::start();
    // The daemon's own session is on the daemon's own provider, and stays there: the row is
    // written only by a switch, and nothing has switched it.
    assert_eq!(world.model_of(&world.session), None);

    let bridge = Bridge::new();
    let started = bridge
        .start(
            &world.dir.path().join("worktree").display().to_string(),
            BRIEF,
            Some("deepseek/deepseek-chat"),
            "writes-allowed",
            None,
        )
        .expect("the bridge starts a session on the model it was asked for");
    let session = started["session"]
        .as_str()
        .expect("a session id")
        .to_string();
    assert_ne!(session, world.session, "a new session, not the daemon's");
    assert_eq!(started["daemon_started"], false, "a JOIN: {started}");
    assert_eq!(started["model"], "deepseek/deepseek-chat", "{started}");
    assert!(
        started["model_said"]
            .as_str()
            .unwrap_or_default()
            .contains("deepseek"),
        "the daemon's own sentence says where the turns go: {started}"
    );

    // **The turn really went there**: the request the provider received names the model the
    // caller asked for, and not the daemon's own.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let first = loop {
        let bodies = world.fake.requests();
        if let Some(b) = bodies.first() {
            break b.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the brief's turn never reached the provider"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        first.contains("deepseek-chat"),
        "the turn went to the model the caller named: {first}"
    );

    // **And the choice is the session's, written down**: a restart of that session comes back on
    // it (`restore_provider_choice`), and the daemon's own session was not moved.
    assert_eq!(
        world.model_of(&session).as_deref(),
        Some("deepseek/deepseek-chat")
    );
    assert_eq!(
        world.model_of(&world.session),
        None,
        "the daemon's own session keeps the daemon's provider"
    );

    // **A name this box cannot serve is refused BEFORE the brief**, so nothing runs on a model
    // nobody asked for, and the refusal names the model and the session it left behind.
    let before = world.fake.requests().len();
    let refused = bridge
        .start(
            &world.dir.path().join("worktree").display().to_string(),
            BRIEF,
            Some("no-such-provider"),
            "writes-allowed",
            None,
        )
        .expect_err("a provider this build does not know is refused");
    assert!(
        refused.contains("no-such-provider") && refused.contains("was not sent"),
        "the refusal names the model and says the brief did not run: {refused}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        world.fake.requests().len(),
        before,
        "no turn was sent on the refused model"
    );
    let _ = world.socket.clone();
}
