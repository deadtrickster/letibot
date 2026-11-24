//! `leticode_send` into a session this bridge does not hold: the room decides consent, the
//! session's own daemon queues the prompt, a turn runs, and the row is stored as the bridge's
//! line. Only the model is a loopback server. One test, because the environment is process-wide.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::tools::Bridge;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use serde_json::{Value, json};

const DAEMON_SESSION: &str = "s-unheld-daemon-session";
const BRIEF: &str = "brief-marker: say hello";
const HELLO: &str = "hello-marker: hello";
const PROMPT: &str = "prompt-marker: say done";
const ANSWER: &str = "answer-marker: done";
const HANDLE: &str = "cc@letibot";
const PATIENCE: Duration = Duration::from_secs(60);

/// A directory that removes itself - a test that leaks a store per run fills the disk, and the run
/// that finds out is not this one.
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

/// A fake provider on a loopback port: the n-th request is answered with the n-th entry of the
/// script, and the last entry answers every request after it.
struct Fake {
    url: String,
}

impl Fake {
    fn serve(script: Vec<String>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut turn = 0usize;
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let _ = read_request(stream.try_clone().expect("clone the socket"));
                let answer = script
                    .get(turn)
                    .or_else(|| script.last())
                    .cloned()
                    .unwrap_or_else(|| "done".to_string());
                turn += 1;
                let _ = write_sse(stream, &answer);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
        }
    }
}

/// Drain one HTTP request.
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

/// One answer, in the provider's wire shape: a delta, a finish reason, the usage block and
/// `[DONE]`.
fn write_sse(mut stream: TcpStream, text: &str) -> std::io::Result<()> {
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{"content": text}}]})
    ));
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

/// The daemon, the fake model and the config they share.
struct World {
    dir: TempDir,
    worktree: PathBuf,
    _socket: PathBuf,
    registry: Arc<Registry>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    fn start() -> World {
        let dir = TempDir::new("prompt-unheld");
        let root = dir.path();
        // Nothing of the operator's may reach this session: a key in their `providers.toml` or a
        // mode row in their store would decide what answers, and the models catalogue in their
        // `~/.cache` what a turn costs.
        std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
        // SAFETY: this test binary runs one test, and the variables are set before the
        // daemon, any thread that reads config, or the bridge exists.
        unsafe {
            std::env::set_var("HOME", root);
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
            std::env::set_var("XDG_DATA_HOME", root.join("data"));
            std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("GLM_API_KEY");
            std::env::remove_var("LETIBOT_MODE");
            std::env::remove_var("LETIBOT_SOCKET");
        }
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        // An empty catalogue: the models table is the host's cache unless it is a fixture.
        std::fs::create_dir_all(root.join("cache/letibot")).expect("cache dir");
        std::fs::write(root.join("cache/letibot/models.json"), "{}").expect("models.json");

        let fake = Fake::serve(vec![HELLO.to_string(), ANSWER.to_string()]);
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/prompt_unheld.rs — a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-test\"\nurl = \"{}\"\n",
                fake.url
            ),
        )
        .expect("providers.toml");

        let socket = root.join("daemon.sock");
        // The bridge finds a folder's daemon by the workspace's hash, so `$LETIBOT_SOCKET` names
        // this one - the same override the shell honours.
        unsafe { std::env::set_var("LETIBOT_SOCKET", &socket) };
        // The operator's hosts file, with nothing in it: this file is about rows, and a host this
        // box does not have would only make `leticode_start` refuse.
        std::fs::write(
            root.join("config/letibot/hosts.toml"),
            "# Written by crates/bridge/tests/prompt_unheld.rs — no hosts.\n",
        )
        .expect("hosts.toml");
        unsafe { std::env::set_var("LETIBOT_HOSTS", root.join("config/letibot/hosts.toml")) };

        let mut cfg = Config::for_this_box(&worktree);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        // No exec backend: a delegated cgroup is apparatus this box may not have, and nothing in
        // this file runs a command. The writable backend is enough for a turn that only talks.
        cfg.allow_bash = false;
        // The byte vocabulary: a session whose turns go to a provider needs no GGUF.
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
        // A window the prompt fits inside. The byte vocabulary counts every byte of the system
        // prompt and the tool schemas as a token, so a session's message zero is ~54k by that
        // measure; a 32k window made the harness try to compact before every turn.
        cfg.context_window = Some(200_000);
        cfg.store = Some(root.join("sessions.db"));
        cfg.socket = socket.clone();
        cfg.session_id = DAEMON_SESSION.into();

        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("a provider session opens on the byte vocabulary"),
        ));
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

        let session = cfg.session_id.clone();
        let registry = Registry::new();
        registry
            .create(session, "", Sessions::wiring(&cfg))
            .expect("a fresh registry takes the session");
        // Bound HERE rather than on the worker thread: the bridge decides whether to start a
        // daemon by connecting, and a socket that appears a moment later would be a race.
        let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
        // Where this daemon's tools run, written where harnessd writes it: `host` is this
        // box, and the record is what `leticode_start` compares its own request against.
        letibot_sessionlog::placement::write(&socket, "host").expect("the record");
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
            _socket: socket,
            registry,
            worker: Some(worker),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // The registry's close is what ends `Daemon::run`, so this is the orderly stop.
        self.registry.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        let _ = &self.dir;
    }
}

/// Poll `who`'s status of `session` until its last message carries `marker` and it is idle.
fn until_said(who: &Bridge, session: &str, marker: &str) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let s: Value = who.status(session).expect("status");
        if s["state"] == json!("idle")
            && s["last_message"]
                .as_str()
                .unwrap_or_default()
                .contains(marker)
        {
            return;
        }
        assert!(Instant::now() < deadline, "never said `{marker}`: {s}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_session_this_bridge_does_not_hold_runs_a_prompt_stored_as_the_bridges_line() {
    let world = World::start();
    // SAFETY: one test in this binary, set before either bridge reads its handle.
    unsafe { std::env::set_var(letibot_bridge::rooms::NAME_ENV, HANDLE) };
    // Another head holds the session: a bridge of another folder, or the operator's TUI.
    let owner = Bridge::new();
    let started = owner
        .start(
            &world.worktree.display().to_string(),
            BRIEF,
            Some("deepseek/deepseek-chat"),
            "writes-allowed",
            None,
            Some(false),
            None,
        )
        .expect("the owner starts a session");
    let session = started["session"].as_str().expect("an id").to_string();
    until_said(&owner, &session, HELLO);

    let room_socket = world.dir.path().join("room.sock");
    let room = letibot_room::server::Server::serve(&room_socket, letibot_room::Room::new())
        .expect("a coordinator");
    let operator = letibot_room::client::Client::connect_as(
        room.path(),
        letibot_room::Speaker::top_level("operator"),
        letibot_room::room_name(&room.name(), ""),
        letibot_room::Seat::Operator,
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("the operator connects");
    let bridge = Bridge::with_room_socket(&room_socket);

    // Another project's session, and no operator word cited: refused, with the reason.
    let refused = bridge
        .send_because(&session, PROMPT, None)
        .expect_err("no consent yet");
    assert!(
        refused.contains("because") && refused.contains(&session),
        "{refused}"
    );

    let said = operator
        .request(&letibot_room::Request::Say {
            id: "op-1".into(),
            room: letibot_room::HOST_ROOM.into(),
            text: format!("@{HANDLE} tell {session} to say done"),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: Default::default(),
        })
        .expect("the operator says it");
    let seq = match said {
        letibot_room::Response::Said { message, .. } => message.seq,
        other => panic!("said: {other:?}"),
    };
    let sent = bridge
        .send_because(&session, PROMPT, Some(seq))
        .expect("the room consents and the daemon queues it");
    assert_eq!(sent["queued"], json!(true), "{sent}");
    assert_eq!(sent["held"], json!(false), "{sent}");
    assert!(
        bridge.status(&session).is_err(),
        "the prompting bridge holds nothing after"
    );

    until_said(&owner, &session, ANSWER);
    let got = owner
        .transcript(&session, None, None, false)
        .expect("the rows");
    let rows = got["rows"].as_array().expect("rows");
    let row = rows
        .iter()
        .find(|r| {
            r["text"]
                .as_str()
                .is_some_and(|t| t.contains("prompt-marker"))
        })
        .unwrap_or_else(|| panic!("no prompt row: {rows:#?}"));
    assert_eq!(row["kind"], json!("user"), "{row}");
    assert_eq!(row["speaker"], json!("peer"), "{row}");
    assert_eq!(row["from"], json!(HANDLE), "{row}");
    drop(operator);
    drop(bridge);
    drop(owner);
    drop(world);
}
