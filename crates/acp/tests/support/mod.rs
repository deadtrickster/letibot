//! The fixture both client scripts drive: a real daemon, a fake provider, a worktree, and the
//! adapter as a child process.
//!
//! harnessd's `Daemon` over a real `Registry` and `Sessions::open_first`, as the bridge's own
//! end-to-end test builds one; only the model is a loopback server. The daemon runs IN this
//! process, so it is bound with `serve_rooted` and a tree no real process descends from - without
//! that, the adapter as its child is judged a tool call and refused the seat.
//
//! Each client script compiles this module into its own binary, so a helper the other script uses
//! is dead code in this one: the allow is the price of one fixture instead of three.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_tools::mode::Mode;
use serde_json::{Value, json};

/// Generous on purpose: every wait is for something a correct build produces in milliseconds, and
/// the assertion is what happened, not how fast.
pub const PATIENCE: Duration = Duration::from_secs(120);

/// One model round, as the fake provider answers it.
#[derive(Clone)]
pub enum Answer {
    /// Prose, in these pieces, so "streamed" is a thing the test can see.
    Say(&'static [&'static str]),
    /// The same, one piece every few hundred milliseconds: slow enough for a client to stop it.
    Slow(&'static [&'static str]),
    /// One tool call, then the turn continues.
    Call {
        name: &'static str,
        arguments: Value,
    },
}

pub struct World {
    pub root: PathBuf,
    pub worktree: PathBuf,
    pub fake: Fake,
    registry: Arc<Registry>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    /// A daemon on a folder's socket, a worktree in a repository, and the model scripted.
    pub fn start(tag: &str, mode: Mode, script: Vec<Answer>) -> World {
        let root = letibot_sessionlog::testing::scratch(tag);
        std::fs::create_dir_all(&root).expect("the scratch root");
        let root = root.canonicalize().expect("a canonical root");
        std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
        std::fs::create_dir_all(root.join("bin")).expect("the binary's directory");
        // Nothing of the operator's may reach this session: a key in their `providers.toml` would
        // decide what answers, and their models catalogue what a turn costs. Both are scratch.
        // SAFETY: each client script is one test in its own binary, and these are set before the
        // daemon, any thread that reads config, or the adapter exists.
        unsafe {
            std::env::set_var("HOME", &root);
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
        std::fs::write(worktree.join("article.md"), article_text()).expect("the article");
        a_repository(&worktree);
        std::fs::create_dir_all(root.join("cache/letibot")).expect("cache dir");
        std::fs::write(root.join("cache/letibot/models.json"), "{}").expect("models.json");

        let fake = Fake::serve(script);
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/acp/tests/support — a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-test\"\nurl = \"{}\"\n",
                fake.url()
            ),
        )
        .expect("providers.toml");

        let socket = root.join("daemon.sock");
        // The adapter finds a folder's daemon by its hash, so `$LETIBOT_SOCKET` names this one -
        // the same override the shell honours, and the only way a test is sure which socket it
        // started. The child process inherits it.
        unsafe { std::env::set_var("LETIBOT_SOCKET", &socket) };

        let mut cfg = Config::for_this_box(&worktree);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        cfg.mode = mode;
        // The byte vocabulary: a session whose turns go to a provider needs no GGUF, which keeps
        // llama.cpp - and every model file on this box - out of the test.
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
        cfg.session_id = format!("{tag}-first-session");

        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("a provider session opens on the byte vocabulary"),
        ));
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

        let registry = Registry::new();
        registry
            .create(cfg.session_id.clone(), "", Sessions::wiring(&cfg))
            .expect("a fresh registry takes the session");
        // The store is the daemon's source of sessions, wired the way the daemon binary wires it:
        // a `session/new` from the client is answered from it, not from this test.
        registry.set_source(
            letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("the store is a source"),
        );
        let daemon = Daemon::serve_rooted(
            registry.clone(),
            &socket,
            letibot_sessionlog::testing::FOREIGN_TREE,
        )
        .expect("the socket binds");
        let worker_registry = registry.clone();
        let worker = std::thread::spawn(move || {
            let mut sessions = Sessions::open_first(parts, cfg, worker_registry)
                .expect("the daemon opens its first session");
            daemon.run(&mut sessions, |_, _| {});
            daemon.shutdown();
        });

        World {
            root,
            worktree,
            fake,
            registry,
            worker: Some(worker),
        }
    }

    /// The passage the ranged prompt names, as an editor's buffer would hold it.
    pub fn passage(&self) -> String {
        article_text()
            .lines()
            .skip(40)
            .take(7)
            .map(|l| format!("{l}\n"))
            .collect()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // The registry's close is what ends `Daemon::run`: the orderly stop, not a kill.
        self.registry.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Sixty lines, so `#L41-L47` names a passage and not the whole file.
pub fn article_text() -> String {
    let mut out = String::new();
    for n in 1..=60 {
        if n == 41 {
            out.push_str("The first transatlantic cable was laid in 1866\n");
        } else if (42..=47).contains(&n) {
            out.push_str(&format!("and line {n} of the record\n"));
        } else {
            out.push_str(&format!("line {n} of the article\n"));
        }
    }
    out
}

/// A repository, because a folder's daemon is found by its git toplevel.
fn a_repository(at: &Path) {
    let run = |args: &[&str]| {
        let out = letibot_tools::testing::git(at)
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
    std::fs::write(at.join("README.md"), "the base\n").expect("write");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "the base commit"]);
}

// ---------------------------------------------------------------------------
// The client: one child process and JSON-RPC lines
// ---------------------------------------------------------------------------

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    /// The real binary, as a client launches it: no arguments, stdio and nothing else.
    pub fn launch(cwd: &Path) -> Client {
        let binary = std::env::var_os("CARGO_BIN_EXE_letibot-acp")
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.join("letibot-acp"));
        let mut child = Command::new(&binary)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The adapter's own log, so a failure shows what it said rather than a hang.
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("{}: {e}", binary.display()));
        let stdin = child.stdin.take().expect("a pipe to the client");
        let stdout = child.stdout.take().expect("a pipe from the client");
        Client {
            child,
            stdin,
            lines: BufReader::new(stdout),
            next: 1,
        }
    }

    /// The handshake and a session, which every script begins with.
    pub fn opened(cwd: &Path) -> (Client, String) {
        let mut client = Client::launch(cwd);
        let answer = client.call("initialize", json!({"protocolVersion": 1}));
        assert_eq!(answer["protocolVersion"], 1, "the schema's stable version");
        assert_eq!(
            answer["agentCapabilities"]["promptCapabilities"]["embeddedContext"], true,
            "a `resource` block in a prompt is what this adapter was built for"
        );
        assert_eq!(answer["agentInfo"]["name"], "letibot");
        let answer = client.call("session/new", json!({"cwd": cwd, "mcpServers": []}));
        let session = answer["sessionId"]
            .as_str()
            .expect("session/new answers an id")
            .to_string();
        assert!(!session.is_empty(), "the daemon named a session");
        (client, session)
    }

    pub fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// A notification: no answer is expected.
    pub fn notify(&mut self, method: &str, params: Value) {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Answer a request the adapter sent, by its id.
    pub fn answer(&mut self, id: Value, result: Value) {
        self.write(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    /// A request answered before the test reads anything else: `initialize` and `session/new`.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        let (message, _) = self.until(id, |_| None);
        assert!(message.get("error").is_none(), "{method} failed: {message}");
        message["result"].clone()
    }

    pub fn write(&mut self, message: Value) {
        let mut line = serde_json::to_string(&message).expect("a message serialises");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("the client writes");
        self.stdin.flush().expect("the client flushes");
    }

    /// Every message until the answer to `id`, and the answer. `answer_to` lets a script answer a
    /// request of the adapter's own while it waits - a permission card is asked mid-turn.
    pub fn until(
        &mut self,
        id: u64,
        mut answer_to: impl FnMut(&Value) -> Option<Value>,
    ) -> (Value, Vec<Value>) {
        let deadline = Instant::now() + PATIENCE;
        let mut seen = Vec::new();
        loop {
            let message = self.read(deadline, id);
            if message.get("method").is_none()
                && message.get("id").and_then(Value::as_u64) == Some(id)
            {
                return (message, seen);
            }
            if message.get("method").is_some()
                && message.get("id").is_some()
                && let Some(result) = answer_to(&message)
            {
                let asked = message["id"].clone();
                self.answer(asked, result);
            }
            seen.push(message);
        }
    }

    /// Every message until the answer to `id`, sending `session/cancel` once `after` chunks of the
    /// reply have arrived: the shape a client's stop button has.
    pub fn until_cancelled(&mut self, id: u64, session: &str, after: usize) -> (Value, Vec<Value>) {
        let deadline = Instant::now() + PATIENCE;
        let mut seen: Vec<Value> = Vec::new();
        let mut chunks = 0usize;
        loop {
            let message = self.read(deadline, id);
            if message.get("method").is_none()
                && message.get("id").and_then(Value::as_u64) == Some(id)
            {
                return (message, seen);
            }
            if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                chunks += 1;
                if chunks == after {
                    self.notify("session/cancel", json!({"sessionId": session}));
                }
            }
            seen.push(message);
        }
    }

    fn read(&mut self, deadline: Instant, waiting_on: u64) -> Value {
        loop {
            assert!(
                Instant::now() < deadline,
                "waited {PATIENCE:?} for the answer to request {waiting_on}"
            );
            let mut line = String::new();
            let read = self.lines.read_line(&mut line).expect("the client reads");
            assert!(
                read > 0,
                "the adapter closed its stdout while request {waiting_on} was open"
            );
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(line.trim()).unwrap_or_else(|e| {
                panic!("the adapter wrote a line that is not a message ({e}): {line}")
            });
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// The model: a loopback server answering the provider's own frames
// ---------------------------------------------------------------------------

pub struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn serve(script: Vec<Answer>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        std::thread::spawn(move || {
            for (round, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { return };
                let body = read_request(stream.try_clone().expect("clone the socket"));
                record.lock().unwrap().push(body);
                let answer = script.get(round).cloned().unwrap_or(Answer::Say(&["done"]));
                let _ = write_sse(stream, &answer);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            seen,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every request body the model was handed, in order.
    pub fn requests(&self) -> Vec<String> {
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

/// The provider's frames: the answer, a finish reason, the usage block, `[DONE]`.
fn write_sse(mut stream: TcpStream, answer: &Answer) -> std::io::Result<()> {
    let mut body = String::new();
    match answer {
        Answer::Say(chunks) => {
            for chunk in *chunks {
                body.push_str(&format!(
                    "data: {}\n\n",
                    json!({"choices":[{"delta":{"content": chunk}}]})
                ));
            }
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
            ));
            return reply(stream, &body);
        }
        Answer::Slow(chunks) => {
            // Written as it goes: a body held until the end would be one chunk however many
            // pieces it carries, and a client cannot stop what has already arrived.
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )?;
            for chunk in *chunks {
                write!(
                    stream,
                    "data: {}\n\n",
                    json!({"choices":[{"delta":{"content": chunk}}]})
                )?;
                stream.flush()?;
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            return Ok(());
        }
        Answer::Call { name, arguments } => {
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"tool_calls":[{
                    "index": 0,
                    "id": "call-1",
                    "function": { "name": name, "arguments": arguments.to_string() },
                }]}}]})
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})
            ));
        }
    }
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[],"usage":{
            "prompt_tokens": 50,
            "completion_tokens": 9,
            "prompt_cache_hit_tokens": 20,
        }})
    ));
    body.push_str("data: [DONE]\n\n");
    reply(stream, &body)
}

/// One whole body, with its length: the shape a provider answers in when it does not stream live.
fn reply(mut stream: TcpStream, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
