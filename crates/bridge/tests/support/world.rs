//! A real daemon in this process, on a fake provider, for the tests that drive `leticode_start`
//! end to end. The provider keeps every request body, so "did a turn run" is a count.
//! The fixture sets process env: one test per binary.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use serde_json::json;

pub const ANSWER: &str = "answer-marker: said";

/// A directory that removes itself.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-mcp-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A gate over the fake's first answer: the turn stays in its round until the test opens it, which
/// is the only way to speak to a session mid-turn on a fake that answers in a millisecond.
#[derive(Clone, Default)]
pub struct Hold(Option<Arc<(Mutex<bool>, std::sync::Condvar)>>);

impl Hold {
    fn armed() -> Hold {
        Hold(Some(Arc::new((
            Mutex::new(false),
            std::sync::Condvar::new(),
        ))))
    }

    pub fn release(&self) {
        let Some(gate) = &self.0 else { return };
        let (open, bell) = &**gate;
        *open.lock().unwrap_or_else(|e| e.into_inner()) = true;
        bell.notify_all();
    }

    fn wait(&self) {
        let Some(gate) = &self.0 else { return };
        let (open, bell) = &**gate;
        let mut open = open.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            open = bell.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// A fake provider on loopback: one sentence per request, every body kept.
pub struct Fake {
    pub url: String,
    seen: Arc<Mutex<Vec<String>>>,
    hold: Hold,
}

impl Fake {
    pub fn serve() -> Fake {
        Fake::serve_held(Hold::default())
    }

    fn serve_held(hold: Hold) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let gate = hold.clone();
        std::thread::spawn(move || {
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let body = read_request(stream.try_clone().expect("clone the socket"));
                record.lock().unwrap().push(body);
                gate.wait();
                let _ = write_sse(stream);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            seen,
            hold,
        }
    }

    pub fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

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
        json!({"choices":[],"usage":{"prompt_tokens": 50, "completion_tokens": 9}})
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
pub struct World {
    pub dir: TempDir,
    pub socket: PathBuf,
    pub session: String,
    pub registry: Arc<Registry>,
    pub fake: Fake,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    pub fn start(tag: &str) -> World {
        World::start_with(tag, Hold::default())
    }

    /// A world whose provider holds its first answer until [`Hold::release`]: the brief's turn runs
    /// until the test says so, which is what makes a prompt sent mid-turn sendable at all.
    pub fn start_held(tag: &str) -> World {
        World::start_with(tag, Hold::armed())
    }

    /// The gate the fake is holding on, for a test that has to open it.
    pub fn hold(&self) -> Hold {
        self.fake.hold.clone()
    }

    fn start_with(tag: &str, hold: Hold) -> World {
        let dir = TempDir::new(tag);
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
        // SAFETY: one test per binary, set before the daemon, the bridge or any config reader.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_DATA_HOME", root.join("data"));
            std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("GLM_API_KEY");
            std::env::remove_var("LETIBOT_MODE");
        }
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let fake = Fake::serve_held(hold);
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/support/world.rs: a fake on loopback.\n\
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
            name: "glm".into(),
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
        cfg.session_id = format!("mcp-{tag}");

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
            session,
            registry,
            fake,
            worker: Some(worker),
        }
    }

    pub fn worktree(&self) -> String {
        self.dir.path().join("worktree").display().to_string()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.registry.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
