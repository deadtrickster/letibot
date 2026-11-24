//! A bridge that drives a session writes that session's board: rows under the seat's handle,
//! reordered and completed, and the model told at its next turn. A watching bridge is refused.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::tools::{self, Bridge};
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use serde_json::{Value, json};

const HANDLE: &str = "cc@driver";
const PATIENCE: Duration = Duration::from_secs(60);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A provider on loopback that says "done" to every round and keeps each request body.
fn fake() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            let body = read_body(stream.try_clone().expect("clone"));
            record.lock().unwrap().push(body);
            let _ = answer(stream);
        }
    });
    (format!("http://127.0.0.1:{port}/v1/chat/completions"), seen)
}

fn read_body(mut s: TcpStream) -> String {
    let mut acc = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
        let Some(at) = acc.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let want = String::from_utf8_lossy(&acc[..at])
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        if acc.len() - (at + 4) >= want {
            return String::from_utf8_lossy(&acc[at + 4..]).into_owned();
        }
    }
    String::from_utf8_lossy(&acc).into_owned()
}

fn answer(mut stream: TcpStream) -> std::io::Result<()> {
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"content":"done"}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":1}}),
    );
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn a_repository(at: &Path) {
    let run = |args: &[&str]| {
        let out = letibot_tools::testing::git(at)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}");
    };
    run(&["init", "-q", "--initial-branch=main"]);
    std::fs::write(at.join("README.md"), "the base\n").expect("write");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "the base commit"]);
}

fn until(bridge: &Bridge, session: &str, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let v = bridge.status(session).expect("status");
        if ready(&v) {
            return v;
        }
        assert!(Instant::now() < deadline, "waited for {what}: {v}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn mine(todos: &Value) -> Vec<(String, String)> {
    todos
        .as_array()
        .expect("a list")
        .iter()
        .filter(|t| t["by"] == format!("Peer {HANDLE}"))
        .map(|t| {
            (
                t["content"].as_str().unwrap_or_default().to_string(),
                t["status"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_driving_bridge_writes_the_board_and_a_watching_one_is_refused() {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = TempDir(
        letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-driver-todo-{}-{n}", std::process::id())),
    );
    let root = dir.0.clone();
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    std::fs::create_dir_all(root.join("cache/letibot")).expect("cache dir");
    std::fs::write(root.join("cache/letibot/models.json"), "{}").expect("models.json");
    let socket = root.join("daemon.sock");
    // SAFETY: one test in this binary, and the variables are set before any thread reads them.
    unsafe {
        std::env::set_var("HOME", &root);
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::set_var(letibot_bridge::rooms::NAME_ENV, HANDLE);
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    a_repository(&worktree);
    let (url, seen) = fake();
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!("[deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"),
    )
    .expect("providers.toml");

    let mut cfg = Config::for_this_box(&worktree);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
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
    cfg.session_id = "driver-daemon-session".into();
    let parts: &'static Parts = Box::leak(Box::new(Parts::load(&cfg).expect("parts load")));
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    registry
        .create(cfg.session_id.clone(), "", Sessions::wiring(&cfg))
        .expect("the registry takes the session");
    registry.set_source(letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("source"));
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    letibot_sessionlog::placement::write(&socket, "host").expect("the placement record");
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        let mut sessions =
            Sessions::open_first(parts, cfg, worker_registry).expect("the first session opens");
        daemon.run(&mut sessions, |_, _| {});
        daemon.shutdown();
    });

    let wt = worktree.display().to_string();
    let bridge = Bridge::new();
    let started = bridge
        .start(
            &wt,
            "start here",
            Some("deepseek/deepseek-chat"),
            "writes-allowed",
            None,
            Some(false),
            None,
        )
        .expect("the bridge starts a session");
    let session = started["session"].as_str().expect("an id").to_string();
    until(&bridge, &session, "the brief's turn", |v| {
        v["state"] == "idle" && v["last_message"].as_str().is_some()
    });

    // Create two rows, then complete the second and name it first.
    let row = |c: &str, s: &str| json!({ "content": c, "status": s });
    let wrote = tools::call(
        &bridge,
        "leticode_todo",
        &json!({ "session": session, "rows": [row("rebase on main", "pending"), row("push the branch", "pending")] }),
    )
    .expect("a driving bridge writes the board");
    assert_eq!(
        mine(&wrote["todos"]),
        [
            ("rebase on main".into(), "pending".into()),
            ("push the branch".into(), "pending".into())
        ],
        "{wrote}"
    );
    let wrote = tools::call(
        &bridge,
        "leticode_todo",
        &json!({ "session": session, "rows": [row("push the branch", "completed")] }),
    )
    .expect("a second write");
    assert_eq!(
        mine(&wrote["todos"]),
        [
            ("push the branch".into(), "completed".into()),
            ("rebase on main".into(), "pending".into())
        ],
        "completed, and moved ahead of the row it did not name: {wrote}"
    );

    // The model reads what the driver did at its next turn.
    let before = seen.lock().unwrap().len();
    bridge.send(&session, "carry on").expect("a line");
    let deadline = Instant::now() + PATIENCE;
    let told = loop {
        let bodies = seen.lock().unwrap().clone();
        if let Some(b) = bodies[before..]
            .iter()
            .find(|b| b.contains("the agent driving this session, changed your todo board"))
        {
            break b.clone();
        }
        assert!(Instant::now() < deadline, "no turn carried the notice");
        std::thread::sleep(Duration::from_millis(25));
    };
    for said in [
        HANDLE,
        "added `rebase on main` (pending)",
        "marked `push the branch` completed",
        "reordered its rows",
    ] {
        assert!(told.contains(said), "`{said}` is in the notice: {told}");
    }

    // A watching bridge is refused, and the board is unchanged.
    let watcher = Bridge::new();
    watcher.attach(&wt, &session, true).expect("a watch");
    let refused = tools::call(
        &watcher,
        "leticode_todo",
        &json!({ "session": session, "rows": [row("something else", "pending")] }),
    )
    .expect_err("a watching head does not write the board");
    assert!(refused.contains("watched, not held"), "{refused}");
    let after = bridge.status(&session).expect("status");
    assert_eq!(mine(&after["todos"]).len(), 2, "{after}");

    drop(watcher);
    drop(bridge);
    registry.close();
    let _ = worker.join();
}
