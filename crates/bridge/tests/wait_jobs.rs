//! A `wait` holds on the session's own background job, and not on the one the model parked.
//!
//! A session idle with a job open used to make `letibot-mcp wait` exit `0` at once, though the
//! job's settlement was going to wake the model and start a turn of its own: the orchestrator read
//! *done* and the package had a turn left in it. The other half is what keeps that usable - a dev
//! server left running is `bash {background: true, wake: "later"}`, the model saying it does not
//! need waking for it, and a `wait` must not sit out its deadline for one.
//!
//! The daemon here is a real one in this process: a real `bash`, a real watcher thread, a real
//! settlement on the session's bell, and the wake the daemon takes from it.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::tools::{self, Bridge};
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use serde_json::{Value, json};

/// The brief, and one marker per turn so a returned message says WHICH turn it was.
const BRIEF: &str = "brief-marker: start the slow one";
const STARTED_ANSWER: &str = "started-answer-marker: the slow one is running";
const WOKEN_ANSWER: &str = "woken-answer-marker: the slow one ended";
const PARK_BRIEF: &str = "park-marker: leave a server running";
const PARKED_ANSWER: &str = "parked-answer-marker: the server is up";
/// A job that ends while the test is watching, and one that does not.
const SHORT_JOB: &str = "sleep 5";
const LONG_JOB: &str = "sleep 300";

/// Generous: every wait here is for something a correct build produces in seconds.
const PATIENCE: Duration = Duration::from_secs(90);

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

/// One answer the fake gives.
#[derive(Clone)]
enum Answer {
    /// A tool call the model proposes: the name and its arguments.
    Call(&'static str, Value),
    /// A sentence, which ends the turn it is said in.
    Say(&'static str),
}

/// A fake provider on loopback, answering the scripted list in order.
struct Fake {
    url: String,
}

impl Fake {
    fn serve(script: Vec<Answer>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut turn = 0usize;
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let _ = read_request(stream.try_clone().expect("clone the socket"));
                let answer = script.get(turn).cloned().unwrap_or(Answer::Say("done"));
                turn += 1;
                let _ = write_sse(stream, &answer);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
        }
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

/// The provider's own frames: a delta (a call or a sentence), a finish reason, usage, and `[DONE]`.
fn write_sse(mut stream: TcpStream, answer: &Answer) -> std::io::Result<()> {
    let mut body = String::new();
    match answer {
        Answer::Call(name, arguments) => {
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"tool_calls":[{
                    "index": 0,
                    "id": "call_wait_job",
                    "function": { "name": name, "arguments": arguments.to_string() },
                }]}}]})
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})
            ));
        }
        Answer::Say(text) => {
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"content": text}}]})
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
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
    worktree: PathBuf,
    registry: Arc<Registry>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    fn start() -> World {
        let dir = TempDir::new("wait-jobs");
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
            // Never the gate's own demand: `LETIBOT_REQUIRE_APPARATUS` stays whatever the run
            // set, so a fixture here cannot turn this binary's skips green.
        }
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        // Six answers, in the order the turns take them: the brief's call, its answer, the wake's
        // answer when the short job ends, then the parked call, its answer, and one more `Say` for
        // a turn that must not happen.
        let fake = Fake::serve(vec![
            Answer::Call("bash", json!({"command": SHORT_JOB, "background": true})),
            Answer::Say(STARTED_ANSWER),
            Answer::Say(WOKEN_ANSWER),
            Answer::Call(
                "bash",
                json!({"command": LONG_JOB, "background": true, "wake": "later"}),
            ),
            Answer::Say(PARKED_ANSWER),
        ]);
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/wait_jobs.rs: a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-test\"\nurl = \"{}\"\n",
                fake.url
            ),
        )
        .expect("providers.toml");

        let socket = root.join("daemon.sock");
        // SAFETY: as above, before anything opens a socket.
        unsafe { std::env::set_var("LETIBOT_SOCKET", &socket) };

        let mut cfg = Config::for_this_box(&worktree);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        cfg.allow_bash = true;
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
        cfg.context_window = Some(32_768);
        cfg.store = Some(root.join("sessions.db"));
        cfg.socket = socket.clone();
        cfg.session_id = "mcp-wait-jobs".into();

        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("a provider session opens on the byte vocabulary"),
        ));
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

        let registry = Registry::new();
        registry
            .create(cfg.session_id.clone(), "", Sessions::wiring(&cfg))
            .expect("a fresh registry takes the session");
        let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
        // The record a joining bridge reads: without it the daemon is placed nowhere it can see,
        // and `leticode_start` refuses to join rather than guess where its tools run. This daemon's
        // tools do run here, so the spelling is the bare `host`.
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
            registry,
            worker: Some(worker),
        }
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

/// Poll `status` until the session says what the turn it just took said, or the patience runs out.
fn settled_with(bridge: &Bridge, session: &str, marker: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let status = bridge
            .status(session)
            .unwrap_or_else(|e| panic!("status failed while waiting for {marker}: {e}"));
        if status["state"] == "idle"
            && status["last_message"]
                .as_str()
                .unwrap_or_default()
                .contains(marker)
        {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for `{marker}`; the session says {status}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_wait_holds_on_a_running_job_and_not_on_the_one_the_model_parked() {
    // The seat names a process backend, so the apparatus is a tree the box must have; absent
    // apparatus is announced rather than asserted about.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let world = World::start();
    let bridge = Bridge::new();
    let tree = world.worktree.display().to_string();
    let started = bridge
        .start(&tree, BRIEF, None, "writes-allowed", None, false)
        .expect("the bridge starts a session");
    let session = started["session"]
        .as_str()
        .expect("a session id")
        .to_string();
    // The brief's turn started the job and ended: that answer, and no more, is what a `wait` must
    // not be satisfied with.
    let idle = settled_with(&bridge, &session, STARTED_ANSWER);
    assert!(
        idle["jobs_open"].as_array().is_some_and(|j| !j.is_empty()),
        "the job the brief started is still running: {idle}"
    );

    // the first half: the wait holds until the WOKEN turn ends
    let began = Instant::now();
    let (code, said) = tools::wait(&tree, &session, Instant::now() + PATIENCE);
    let took = began.elapsed();
    assert_eq!(code, 0, "a turn ended: {said}");
    assert!(
        said.contains(WOKEN_ANSWER),
        "the wait returned the turn the settlement woke, not the one that started the job \
         (after {took:?}): {said}"
    );
    assert!(
        took >= Duration::from_millis(2500),
        "it held on the running job rather than answering at once (took {took:?})"
    );

    // the second half: a parked job does not hold it
    bridge
        .send(&session, PARK_BRIEF)
        .expect("a second line into the session");
    let parked = settled_with(&bridge, &session, PARKED_ANSWER);
    assert!(
        parked["jobs_open"]
            .as_array()
            .is_some_and(|j| !j.is_empty()),
        "the parked job is still running, so what follows is about the declaration: {parked}"
    );
    let began = Instant::now();
    let (code, said) = tools::wait(&tree, &session, Instant::now() + Duration::from_secs(20));
    let took = began.elapsed();
    assert_eq!(code, 0, "a turn ended: {said}");
    assert!(
        said.contains(PARKED_ANSWER),
        "a job the model parked must not hold a wait: {said}"
    );
    assert!(
        took < Duration::from_secs(10),
        "it did not sit out its deadline for a job nothing is waiting on (took {took:?})"
    );
}
