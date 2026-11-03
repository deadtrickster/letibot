//! A `wait` holds on a session's own RUNNING CHILD, not only on its background jobs.
//!
//! R107 made a running job hold a `wait` (`wait_jobs.rs`, `wait_activity.rs`); a subagent was
//! left out. A session that starts a child with `task`, ends its turn saying it is holding its
//! report until the child answers, is in exactly the state a job leaves it in: the child's
//! settlement reaches its model, and the turn that says it is the session acting again. A wait
//! that answered *the turn ended* there told an orchestrator a package was done with a turn
//! left in it, and the work came back as a message nobody was waiting for (three sightings on
//! 2026-11-02, on a build that already had R107).
//!
//! The daemon here is a real one in this process: a real `task` call, a real child session, a
//! real watcher thread on the parent's slot, and the wake the daemon takes from the settlement.

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

/// The parent's brief, and one marker per turn so a returned message says WHICH turn it was.
const BRIEF: &str = "brief-marker: start the audit";
const STARTED_ANSWER: &str = "started-answer-marker: holding the report until the audit answers";
const WOKEN_ANSWER: &str = "woken-answer-marker: the audit answered";
/// The subtask the parent's `task` call hands the child. The second line is the MARKER, and it is
/// on the second line on purpose: only the child is handed the prompt, and `task`'s own result
/// quotes its FIRST line back into the parent's transcript - so the marker is in the child's
/// request and nowhere in the parent's.
const SUBTASK: &str =
    "read the code and report what is wrong\nchild-marker: this is the audit child";
const CHILD_MARKER: &str = "child-marker: this is the audit child";
/// What the child answers, on its own turn.
const CHILD_ANSWER: &str = "child-answer-marker: three things are wrong";
/// The orchestrator's own order, which is the one production takes: a line in, then a `wait` with no
/// settling window in between - so the wait attaches while the turn it is about is still running.
const SECOND_BRIEF: &str = "second-brief-marker: start the second audit";
const HOLDING_2: &str = "second-holding-marker: holding again until the second audit answers";
const WOKEN_2: &str = "second-woken-marker: the second audit answered";
const CHILD_ANSWER_2: &str = "second-child-answer-marker: one thing is wrong";
/// Generous, and it spans a loaded box rather than timing anything: the parent's turn ends in
/// milliseconds, so this is what makes *the child is still running* a state the test stands in.
const CHILD_TAKES: Duration = Duration::from_secs(10);
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

/// A fake provider on loopback, routing by WHOSE request it is: the child's prompt carries the
/// marker, and a parent's turn never does. Each side has its own list, taken in order.
struct Fake {
    url: String,
}

impl Fake {
    fn serve(parent: Vec<Answer>, child: Vec<Answer>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut parent_turn = 0usize;
            let mut child_turn = 0usize;
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                // One thread per connection: the child's turn is ANSWERED SLOWLY, and a server that
                // took its connections one at a time would hold the socket the parent's next round
                // needs - which would make the parent's turn end after the child's and prove nothing.
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // `/props` is not a turn and takes no answer: a spawn asks the parent's own
                // endpoint for its window, which is a fact about the spawn path and not about the
                // model - a fake that answered it with a turn would hand the next round the wrong one.
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    return;
                }
                // **Whose request this is, and the brief is what tells them apart**: the marker
                // alone does not, because the parent's own `task` CALL is replayed in every request
                // after it, arguments and all - so the parent carries the child's words too.
                let is_child = body.contains(CHILD_MARKER) && !body.contains(BRIEF);
                let answer = if is_child {
                    let a = child
                        .get(child_turn)
                        .cloned()
                        .unwrap_or(Answer::Say("done"));
                    child_turn += 1;
                    std::thread::sleep(CHILD_TAKES);
                    a
                } else {
                    let a = parent
                        .get(parent_turn)
                        .cloned()
                        .unwrap_or(Answer::Say("done"));
                    parent_turn += 1;
                    a
                };
                let _ = write_sse(stream, &answer);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
        }
    }
}

/// Drain one HTTP request and hand back its request line and its body.
fn read_request(mut s: TcpStream) -> (String, String) {
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
    let first = String::from_utf8_lossy(&acc)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let body = match head_end {
        Some(at) => String::from_utf8_lossy(&acc[at + 4..]).into_owned(),
        None => String::from_utf8_lossy(&acc).into_owned(),
    };
    (first, body)
}

/// The answer a spawn's window question gets: the server's own `/props`.
fn write_props(mut stream: TcpStream) -> std::io::Result<()> {
    const BODY: &str = "{\"default_generation_settings\":{\"n_ctx\":32768}}";
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    )?;
    stream.flush()
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
                    "id": "call_wait_child",
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
        let dir = TempDir::new("wait-children");
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
        // The parent's turns, in the order they take them: the brief's `task` call, the answer that
        // ends the turn with the child still running, and the answer the child's settlement wakes.
        let fake = Fake::serve(
            vec![
                Answer::Call("task", json!({"prompt": SUBTASK})),
                Answer::Say(STARTED_ANSWER),
                Answer::Say(WOKEN_ANSWER),
                // And the second brief's: the same call, and the answer the wait must not stop at.
                Answer::Call("task", json!({"prompt": SUBTASK})),
                Answer::Say(HOLDING_2),
                Answer::Say(WOKEN_2),
            ],
            // And the child's: one answer each, said slowly.
            vec![Answer::Say(CHILD_ANSWER), Answer::Say(CHILD_ANSWER_2)],
        );
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/wait_children.rs: a fake on loopback.\n\
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
        // `leticode`: unconfined, and it names `task`. `coder` names it too and needs a confined
        // backend this box may not have; the child re-seats to `coder` and inherits this session's
        // whole-host standing from its own row, which is what keeps the spawn from asking for one.
        cfg.seat = Seat::Leticode;
        // No `bash` anywhere in this test, and that is not tidiness: a whole-host backend that can
        // start a PROCESS needs a process-lifetime mechanism (`/sys/fs/cgroup` on Linux), which a
        // test box may not have - and the child's own seat is `coder`, whose turn here says words.
        // A backend that only reads and writes is one this test can open anywhere.
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
        // A window, because a child is planned against one - a spawn with none is refused by name.
        cfg.context_window = Some(32_768);
        cfg.store = Some(root.join("sessions.db"));
        cfg.socket = socket.clone();
        cfg.session_id = "mcp-wait-children".into();

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
        // and `leticode_start` refuses to join rather than guess where its tools run.
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
/// Every DISTINCT answer is printed: a failure here is about which state the daemon was in, and a
/// driver that only prints the last one hides the transition being tested.
fn settled_with(bridge: &Bridge, session: &str, marker: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    let mut last = String::new();
    loop {
        let status = bridge
            .status(session)
            .unwrap_or_else(|e| panic!("status failed while waiting for {marker}: {e}"));
        let now = status.to_string();
        if now != last {
            println!(
                "[{:?}] state={} activity={} waiting_on={} jobs_open={} last={}",
                deadline.saturating_duration_since(Instant::now()),
                status["state"],
                status["activity"],
                status["waiting_on"],
                status["jobs_open"],
                status["last_message"],
            );
            last = now;
        }
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
fn a_wait_holds_on_a_running_child_and_returns_after_the_turn_it_wakes() {
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

    // The brief's turn started the child and ended: that answer, and no more, is what a `wait` must
    // not be satisfied with. The driver polls for it rather than sleeping a guessed window.
    let idle = settled_with(&bridge, &session, STARTED_ANSWER);
    // **And the daemon must say so**: a running child is what this session is waiting on, in the
    // words a head draws. A build that reads `quiet` here answers the wait inside its grace, and
    // this assertion is the one that says which half broke.
    let waiting: Vec<String> = idle["waiting_on"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| w.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        waiting.iter().any(|w| w.starts_with("child ")),
        "the daemon does not count a running child among what this session waits on: {waiting:?} \
         from {idle}"
    );

    // The child answers at [`CHILD_TAKES`], and the settlement wakes a turn that says so: this is
    // the turn the wait must be answered with, and the only one.
    let began = Instant::now();
    let (code, said) = tools::wait(&tree, &session, Instant::now() + PATIENCE);
    let took = began.elapsed();
    assert_eq!(code, 0, "a turn ended: {said}");
    assert!(
        said.contains(WOKEN_ANSWER),
        "the wait returned the turn that STARTED the child, not the one its settlement woke \
         (after {took:?}): {said}"
    );
    assert!(
        took >= Duration::from_secs(3),
        "it did not hold on the running child at all (took {took:?}): {said}"
    );

    // ---- the second half: the ORDER production takes it in -----------------------------------
    // Everything above waited for the turn to settle first. An orchestrator does not: it writes the
    // line and runs `wait` at once, so the wait attaches while the turn it is about is still
    // running - and a wait that answers inside its start grace is exactly the sighting.
    bridge
        .send(&session, SECOND_BRIEF)
        .expect("a second line into the session");
    let began = Instant::now();
    let (code, said) = tools::wait(&tree, &session, Instant::now() + PATIENCE);
    let took = began.elapsed();
    assert_eq!(code, 0, "a turn ended: {said}");
    assert!(
        said.contains(WOKEN_2),
        "a wait attached while the turn was running answered inside its grace instead of holding \
         on the child that turn started (after {took:?}): {said}"
    );
    assert!(
        took >= Duration::from_secs(3),
        "it did not hold on the running child (took {took:?}): {said}"
    );
}
