//! **The bridge against a real daemon** — the end-to-end test, and what it is made of.
//!
//! # A daemon, not a stub of one
//!
//! The daemon here is harnessd's own `Daemon::serve` over a real `Registry` and a real
//! `Sessions::open_first`, exactly as its integration tests build one (`slash.rs`,
//! `steering_survives_retry.rs`): a socket, the head protocol, the worker loop, the gate and
//! the turn loop are all the shipped ones. What is replaced is **the model**, by a loopback
//! HTTP server answering the OpenAI frames the provider parses — which is the only piece a
//! test cannot have and must not reach for (`fake_provider.rs` is the same fake one crate
//! down). Nothing here talks to 127.0.0.1:8080, to a provider on the network, or to a GGUF:
//! the session opens on the byte vocabulary, which is the vocabulary a provider session uses.
//!
//! # What is asserted, and where each fact is pinned
//!
//! | fact | measured by |
//! |---|---|
//! | `start` creates a session and sends the brief | the model's own request body |
//! | **the brief runs in the session the answer names** | the daemon's own registry, session by session |
//! | status goes `running` then `idle` with the last message | `leticode_status` |
//! | **the brief's report comes back as fields** | `leticode_status.report` |
//! | **`wait` on a session that never turned is a timeout** | the verb, in this process |
//! | a gated call reaches the bridge as an ask | `leticode_asks`, with the tool and the ladder |
//! | an answer the ask never offered is refused, with the list | `leticode_answer` |
//! | an offered answer settles it, and the turn continues | the third model request and the second turn's answer |
//! | **a tool call and the job it started are entries** | `leticode_events` |
//! | `wait` exits 10 on a pending ask and 0 on a turn end | the verb, in this process |
//! | `diff` reads the branch against its merge base | a real git repository |
//! | `stop` interrupts and detaches | the daemon's registry no longer lists a head |
//!
//! # What it deliberately does not cover
//!
//! **`leticode_start` starting a daemon**: the daemon here is already up, so the branch
//! that spawns `harnessd` is not taken. Spawning a second daemon inside a test would be the
//! one thing this suite must never do — a stray `harnessd` on the box outlives the run — and
//! the spawn itself is asserted where it can be: `daemon.rs`'s unit tests pin the key, the
//! path and the flags against the launcher's own values.

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

/// The brief, and the marker the fake's first answer carries — so the message the bridge
/// reports can be traced to the turn that produced it.
const BRIEF: &str = "brief-marker: wire up the bridge";
const FIRST_ANSWER: &str = "first-answer-marker: the brief is done";
/// **The first answer is the brief's report**, in the shape the design asks for: four
/// headings and a commit, so `leticode_status` is parsed on a real turn's row rather than
/// on a string a test wrote for the parser.
const REPORT: &str = "\
## What I built

first-answer-marker: the brief is done — `crates/bridge/src/report.rs` and the ring in \
`head.rs`.

## Test results

`cargo test --offline -p letibot-bridge`: every test passed.

## What I could not do

Nothing the fake provider could tell me about: the job this turn started is still running.

## Contracts

None.

Commit `7a1c0de`, on branch `bridge-events`.
";
/// The second turn's job: a command the gate asks about at `writes-allowed`.
const SECOND_ANSWER: &str = "second-answer-marker: after the denial";
/// What the model says once the backgrounded call has come back with a handle.
const JOB_ANSWER: &str = "job-answer-marker: the slow one is running in the background";
const THIRD_ANSWER: &str = "third-answer-marker: one more turn";
/// The backgrounded command. Two halves and both for a reason: `git push` is what makes the
/// gate ask at `writes-allowed` (a bare `sleep` is a command the classifier admits without
/// asking), and `sleep 30` keeps the job running past the end of the test — a job that
/// settled mid-test would start an unprompted turn of its own, and a test that raced an
/// unprompted turn is a test that passes on Tuesdays.
const SLOW_COMMAND: &str = "git push origin bridge; sleep 30";

/// Generous on purpose: every wait here is for something a correct build produces in
/// milliseconds, and the assertion is what happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(60);

/// A directory that removes itself — a test that leaks a store per run fills the disk, and
/// the run that finds out is not this one.
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

// ---------------------------------------------------------------------------
// The model: a loopback server answering the provider's own frames
// ---------------------------------------------------------------------------

/// One answer the fake gives, in the provider's wire shape.
#[derive(Clone)]
enum Answer {
    /// Prose, then a normal stop.
    Say(&'static str),
    /// One `bash` call — the shape the gate asks about at `writes-allowed`, and a command
    /// it can refuse without running anything.
    AskBash,
    /// One `bash` call the model asks to run in the **background** — the shape that leaves a
    /// job behind, and the only event that says a job started.
    StartJob,
    /// The provider refusing the round: an HTTP error body, which is a turn that FAILS
    /// rather than one that ends. `http_retries = 0` in the fixture, so one refusal is the
    /// answer and not the first of six.
    Refuse,
}

/// A fake provider on a loopback port, answering the scripted list in order.
///
/// It keeps the request bodies, because *what the model was handed* is half of what the
/// bridge promises: a brief that never reached the model would still produce a turn here.
struct Fake {
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
            let mut turn = 0usize;
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let body = read_request(stream.try_clone().expect("clone the socket"));
                // The first answer is held back a moment, so the test can see the session
                // `running` at all: a turn answered in under a millisecond is a turn no
                // assertion about `running` could ever meet, and asserting it against a
                // faster fake would be asserting a race.
                if turn == 0 {
                    std::thread::sleep(Duration::from_millis(400));
                }
                let answer = script.get(turn).cloned().unwrap_or(Answer::Say("done"));
                turn += 1;
                record.lock().unwrap().push(body);
                let _ = write_sse(stream, &answer);
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

/// The provider's frames: deltas, a finish reason, the usage block, and `[DONE]`.
fn write_sse(mut stream: TcpStream, answer: &Answer) -> std::io::Result<()> {
    let mut body = String::new();
    match answer {
        Answer::Refuse => {
            let body = r#"{"error":{"message":"the fake provider refuses this round","type":"invalid_request_error"}}"#;
            return write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
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
        Answer::AskBash => {
            let arguments = json!({ "command": "git push origin bridge" }).to_string();
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"tool_calls":[{
                    "index": 0,
                    "id": "call_bridge_1",
                    "function": { "name": "bash", "arguments": arguments },
                }]}}]})
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})
            ));
        }
        Answer::StartJob => {
            let arguments = json!({ "command": SLOW_COMMAND, "background": true }).to_string();
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"tool_calls":[{
                    "index": 0,
                    "id": "call_bridge_job",
                    "function": { "name": "bash", "arguments": arguments },
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

/// A git repository with one commit on `main` and one on `bridge`, so `leticode_diff` has a
/// merge base and a branch's own work to read.
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
    run(&["checkout", "-q", "-b", "bridge"]);
    std::fs::write(at.join("work.txt"), "the branch's work\n").expect("write");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "the branch's own work"]);
}

/// The daemon, the fake model and the config they share.
struct World {
    dir: TempDir,
    worktree: PathBuf,
    socket: PathBuf,
    session: String,
    registry: Arc<Registry>,
    fake: Fake,
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

        // **One entry per model round, in the order the turns take them.** The deny turn
        // spends two (the call, then the answer past it), the job turn two more (the call
        // it is asked about, then its answer), and the last two are one turn each.
        let fake = Fake::serve(vec![
            Answer::Say(REPORT),
            Answer::AskBash,
            Answer::Say(SECOND_ANSWER),
            Answer::StartJob,
            Answer::Say(JOB_ANSWER),
            Answer::Say(THIRD_ANSWER),
            Answer::Refuse,
        ]);
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/end_to_end.rs — a fake on loopback.\n\
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
        cfg.session_id = "mcp-daemon-session".into();

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
            daemon.run(&mut sessions, |_, _, _| {});
            daemon.shutdown();
        });

        World {
            dir,
            worktree,
            socket,
            session,
            registry,
            fake,
            worker: Some(worker),
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

/// Poll `status` until it says what we are waiting for, or the patience runs out.
fn until<F>(bridge: &Bridge, session: &str, what: &str, mut ready: F) -> Value
where
    F: FnMut(&Value) -> bool,
{
    let deadline = Instant::now() + PATIENCE;
    let mut last;
    loop {
        last = match bridge.status(session) {
            Ok(v) => v,
            Err(e) => panic!("status failed while waiting for {what}: {e}"),
        };
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

/// Poll `leticode_events` until one entry matches, and hand that entry back.
///
/// A page rather than a predicate on the whole answer, because the fact being asserted is
/// always *one entry appeared*: the ring is a stream and the interesting question is never
/// about the shape of the page.
fn until_entry<F>(bridge: &Bridge, session: &str, what: &str, mut want: F) -> Value
where
    F: FnMut(&Value) -> bool,
{
    let deadline = Instant::now() + PATIENCE;
    let mut last;
    loop {
        last = match bridge.events(session, None, None) {
            Ok(v) => v,
            Err(e) => panic!("events failed while waiting for {what}: {e}"),
        };
        let entries = last["entries"].as_array().cloned().unwrap_or_default();
        if let Some(found) = entries.into_iter().find(&mut want) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}; the ring says {last}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// `status.state` is one string; compare against it rather than against the whole object.
fn state_is(v: &Value, want: &str) -> bool {
    v["state"].as_str() == Some(want)
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// **The whole bridge, over the wire, once.**
///
/// One test rather than several because the parts are one story: the brief's turn, the ask
/// the gate raises on the second turn, the answer that settles it, and the two wake-ups a
/// background `wait` would deliver. Split apart, each would need its own daemon and its own
/// fake, and none of them would be able to say that a session survives an ask.
#[test]
fn a_bridge_holds_a_session_over_the_gate_and_wakes_a_caller() {
    let world = World::start();
    assert!(
        letibot_sessionlog::client::HeadClient::attach(
            &world.socket,
            &world.session,
            0,
            "remote",
            "probe",
            Default::default(),
        )
        .is_ok(),
        "the daemon must be serving before the bridge is asked to start anything"
    );
    let bridge = Bridge::new();

    // --- start: a session, the brief, and a turn that runs --------------------
    let started = bridge
        .start(
            &world.worktree.display().to_string(),
            BRIEF,
            Some("deepseek/deepseek-chat"),
            "writes-allowed",
        )
        .expect("the bridge starts a session");
    let session = started["session"]
        .as_str()
        .expect("a session id")
        .to_string();
    assert_ne!(
        session, world.session,
        "a new session, not the daemon's own"
    );
    assert_eq!(started["mode"], "writes-allowed", "{started}");
    assert_eq!(started["daemon_started"], false, "{started}");
    // **The join path, and the one that broke**: a daemon that was already up has a session
    // of somebody else's, so a new one is created and the head is on it before the brief is
    // sent. (The other branch — a daemon this call started — is the one no test may take,
    // because a test that spawns `harnessd` leaves one running.)
    assert_eq!(started["session_opened"], "new", "{started}");

    // `running` while the fake holds its first answer back for 400 ms.
    let running = until(&bridge, &session, "the first turn to start", |v| {
        state_is(v, "running")
    });
    assert_eq!(running["last_message"], "", "{running}");

    // `idle` **with the answer in it**, not `idle` on its own: between two rounds of one
    // turn the session is briefly idle with the previous round's words still on it, and a
    // predicate that accepted that would be asserting the turn had ended when it had not.
    let idle = until(&bridge, &session, "the first turn to end", |v| {
        state_is(v, "idle")
            && v["last_message"]
                .as_str()
                .unwrap_or_default()
                .contains(FIRST_ANSWER)
    });
    let said = idle["last_message"].as_str().unwrap_or_default();
    assert!(said.contains(FIRST_ANSWER), "{idle}");
    assert!(
        idle["tokens"]["prompt"].as_u64().unwrap_or(0) > 0,
        "the meter is the last turn's: {idle}"
    );
    assert!(
        idle["cost_usd"].is_null(),
        "this fake names no prices, so a metered turn is UNPRICED and not free — the field \
         is present and null: {idle}"
    );
    // And the brief really was handed to the model.
    let first = world.fake.requests();
    assert!(
        first[0].contains("brief-marker"),
        "the brief never reached the model: {}",
        first[0]
    );

    // --- the brief ran in the session the answer names -----------------------
    // **The defect this pins, in the operator's words**: a freshly started daemon opens its
    // own first session, the answer named a second one, and the brief ran where nothing was
    // watching. `leticode_status` cannot prove this on its own — it reports what its own
    // head holds — so the daemon's registry is asked which session has the rows.
    assert_eq!(
        idle["session"],
        session.as_str(),
        "status answers about the session the answer named: {idle}"
    );
    let briefs = world.registry.list();
    let answered = briefs
        .iter()
        .find(|b| b.session_id == session)
        .unwrap_or_else(|| panic!("the answered session is in the daemon: {briefs:?}"));
    assert!(
        answered.status.items > 0,
        "the brief's rows are in the session `leticode_start` answered: {answered:?}"
    );
    let daemons_own = briefs
        .iter()
        .find(|b| b.session_id == world.session)
        .unwrap_or_else(|| panic!("the daemon's own first session is still there: {briefs:?}"));
    assert_eq!(
        daemons_own.status.items, 0,
        "and the daemon's own first session ran nothing: a brief that landed there would be \
         a turn nobody's reply is about: {daemons_own:?}"
    );

    // --- the report comes back as data --------------------------------------
    let report = idle
        .get("report")
        .unwrap_or_else(|| panic!("the brief's report, parsed into fields: {idle}"));
    assert!(
        report["built"]
            .as_str()
            .unwrap_or_default()
            .contains(FIRST_ANSWER),
        "{report}"
    );
    assert!(
        report["tested"]
            .as_str()
            .unwrap_or_default()
            .contains("cargo test --offline -p letibot-bridge"),
        "{report}"
    );
    assert!(
        report["could_not"]
            .as_str()
            .unwrap_or_default()
            .contains("still running"),
        "{report}"
    );
    assert!(
        report["contracts"]
            .as_str()
            .unwrap_or_default()
            .starts_with("None."),
        "{report}"
    );
    assert_eq!(
        report["commits"],
        json!(["7a1c0de"]),
        "the sha the report named: {report}"
    );

    // --- wait on a session that has never turned: a timeout, not an end ------
    // **The other defect from the first real run.** A session with no turn behind it has
    // nothing to report as ended, so `0 — the turn ended` after the grace window is a lie
    // the orchestrator reads as *the brief is done*. The daemon's own first session has run
    // nothing here, which is exactly that session.
    let began = Instant::now();
    let (code, out) = tools::wait(
        &world.worktree.display().to_string(),
        &world.session,
        Instant::now() + Duration::from_secs(6),
    );
    assert_eq!(
        code, 30,
        "a session that has never run a turn times out rather than reporting one ended: {out}"
    );
    assert!(out.is_empty(), "a timeout has no payload: {out}");
    assert!(
        began.elapsed() >= Duration::from_secs(5),
        "it waited out the grace ({:?}) instead of answering in it",
        began.elapsed()
    );

    // --- the gate asks, and the bridge is the head it asks -------------------
    bridge
        .send(&session, "now push the branch")
        .expect("a second line into the running session");
    let asking = until(&bridge, &session, "the gate's ask", |v| {
        state_is(v, "asking")
    });
    assert_eq!(asking["state"], "asking", "{asking}");

    let asks = bridge.asks(&session).expect("the asks");
    let ask = asks["asks"]
        .as_array()
        .and_then(|a| a.first())
        .expect("one open ask")
        .clone();
    assert_eq!(ask["kind"], "permission", "{ask}");
    assert_eq!(
        ask["tool"], "bash",
        "the call's name, joined from the log: {ask}"
    );
    assert!(
        ask["target"]
            .as_str()
            .unwrap_or_default()
            .contains("git push"),
        "the card names the command: {ask}"
    );
    let offered: Vec<(String, String)> = ask["options"]
        .as_array()
        .expect("a ladder")
        .iter()
        .filter_map(|o| {
            Some((
                o["option"].as_str()?.to_string(),
                o["kind"].as_str()?.to_string(),
            ))
        })
        .collect();
    let options: Vec<String> = offered.iter().map(|(o, _)| o.clone()).collect();
    assert!(!options.is_empty(), "a permission ask has an answer: {ask}");
    assert!(
        offered.iter().any(|(_, kind)| kind.starts_with("reject")),
        "a bridge must be able to refuse, and the ladder`s own kinds say which \
         option that is: {ask}"
    );
    let ask_id = ask["ask"].as_str().expect("an ask id").to_string();

    // --- wait: an ask pending is exit 10 ------------------------------------
    let (code, out) = tools::wait(
        &world.worktree.display().to_string(),
        &session,
        Instant::now() + Duration::from_secs(30),
    );
    assert_eq!(code, 10, "a pending ask is the wake-up: {out}");
    let payload: Value = serde_json::from_str(&out).expect("the ask is JSON on stdout");
    assert_eq!(payload["ask"], ask_id, "{payload}");

    // --- answer: only an option the ask offered ----------------------------
    let refused = bridge
        .answer(&session, &ask_id, "allow_everything_forever", None)
        .expect_err("an option nobody offered");
    assert!(refused.contains("allow_everything_forever"), "{refused}");
    for option in &options {
        assert!(
            refused.contains(option),
            "the refusal lists {option}: {refused}"
        );
    }

    let reject = offered
        .iter()
        .find(|(_, kind)| kind == "reject_once")
        .map(|(o, _)| o.clone())
        .expect("a ladder offers a plain refusal");
    bridge
        .answer(&session, &ask_id, &reject, Some("not from the bridge"))
        .expect("an offered option settles the ask");
    let after = until(
        &bridge,
        &session,
        "the turn to continue past the denial",
        |v| {
            state_is(v, "idle")
                && v["last_message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(SECOND_ANSWER)
        },
    );
    assert!(
        after["last_message"]
            .as_str()
            .unwrap_or_default()
            .contains(SECOND_ANSWER),
        "{after}"
    );
    assert_eq!(&after["pending_asks"], &json!([]), "{after}");
    // The turn went back to the model with the denial in it, which is what makes the third
    // request the proof that the answer reached the gate rather than the bridge's cache.
    assert_eq!(
        world.fake.requests().len(),
        3,
        "the turn continued after the answer"
    );
    // **An ordinary answer carries no report field**, which is the other half of "the report
    // as data": a status answer with four empty fields would leave a reviewer unable to tell
    // *nothing was produced* from *the parser failed*.
    assert!(
        after.get("report").is_none(),
        "an answer that is not the report has no report field: {after}"
    );

    // --- a backgrounded call is a job, and the progress tool sees it --------
    // The ring's whole point is that an orchestrator can follow a turn without the
    // transcript: the call's name and the command it ran, and the job it left behind. The
    // job is `sleep 30`, so its ending is never part of this run — the *started* entry is
    // the one an attached caller reads while the model waits for its next round.
    bridge
        .send(&session, "and start the slow one in the background")
        .expect("a fourth line");
    let asking = until(&bridge, &session, "the gate's ask about the job", |v| {
        state_is(v, "asking")
    });
    assert_eq!(asking["state"], "asking", "{asking}");
    let asks = bridge.asks(&session).expect("the asks");
    let ask = asks["asks"]
        .as_array()
        .and_then(|a| a.first())
        .expect("one open ask")
        .clone();
    assert!(
        ask["target"]
            .as_str()
            .unwrap_or_default()
            .contains(SLOW_COMMAND),
        "the card names the command the model wants to background: {ask}"
    );
    assert!(
        ask["target"]
            .as_str()
            .unwrap_or_default()
            .contains("git push"),
        "and the command is the one the gate asks about: {ask}"
    );
    let allow = ask["options"]
        .as_array()
        .expect("a ladder")
        .iter()
        .find_map(|o| {
            o["kind"]
                .as_str()
                .filter(|k| k.starts_with("allow"))
                .and_then(|_| o["option"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("a ladder a bridge may accept offers an allow: {ask}"));
    bridge
        .answer(
            &session,
            ask["ask"].as_str().expect("an ask id"),
            &allow,
            None,
        )
        .expect("an offered option admits the call");

    let tool = until_entry(&bridge, &session, "the call to appear", |e| {
        e["kind"] == "tool" && e["text"].as_str().is_some_and(|t| t.contains(SLOW_COMMAND))
    });
    assert_eq!(tool["name"], "bash", "the entry names the tool: {tool}");
    assert_eq!(tool["state"], "proposed", "{tool}");
    let job = until_entry(&bridge, &session, "the job the call started", |e| {
        e["kind"] == "job" && e["state"] == "started"
    });
    let handle = job["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| panic!("a job entry names the handle: {job}"));
    assert!(
        job["ms"].is_u64(),
        "and how long it ran in the foreground before it was backgrounded: {job}"
    );
    assert!(
        handle.starts_with('j'),
        "the handle is the one `job_output` takes: {handle}"
    );
    // The turn then finished past the admitted call, which is what says the job really
    // started rather than the ask being answered and the turn abandoned.
    let past = until(&bridge, &session, "the turn to finish past the job", |v| {
        state_is(v, "idle")
            && v["last_message"]
                .as_str()
                .unwrap_or_default()
                .contains(JOB_ANSWER)
    });
    assert!(
        past["pending_asks"] == json!([]),
        "the card is settled: {past}"
    );
    // **The cursor is what makes the ring a stream.** The next answer, asked from the cursor
    // this one handed back, holds only what happened since.
    let all = bridge.events(&session, None, None).expect("the events");
    let next = all["next"].as_u64().expect("a cursor");
    assert_eq!(
        all["dropped"],
        Value::Null,
        "a turn that fits in the ring dropped nothing: {all}"
    );
    let rest = bridge
        .events(&session, Some(next), None)
        .expect("the events since the cursor");
    assert_eq!(
        &rest["entries"],
        &json!([]),
        "nothing new past the cursor: {rest}"
    );
    assert_eq!(
        rest["next"],
        json!(next),
        "and the cursor comes back: {rest}"
    );
    // Every entry is compact: a cursor, a kind, and one line — never a payload.
    for entry in all["entries"].as_array().expect("entries") {
        assert!(entry["cursor"].is_u64(), "{entry}");
        assert!(entry["kind"].is_string(), "{entry}");
        let text = entry["text"].as_str().unwrap_or_default();
        assert!(
            text.chars().count() <= 160,
            "an entry is one line, not a payload: {entry}"
        );
        assert!(!text.contains('\n'), "and not a multi-line one: {entry}");
    }

    // --- send + wait: a turn ending is exit 0 ------------------------------
    bridge
        .send(&session, "one more turn please")
        .expect("the third line");
    let (code, out) = tools::wait(
        &world.worktree.display().to_string(),
        &session,
        Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(code, 0, "a turn ended: {out}");
    assert!(out.contains(THIRD_ANSWER), "{out}");

    // --- wait: a timeout with nothing to report is exit 30 ------------------
    // The turn is over and nothing is running, so the only thing the verb can do is wait
    // out its own deadline. It is given one shorter than the grace window on purpose: a
    // session with nothing to do is a timeout, not a turn that never started.
    let (code, out) = tools::wait(
        &world.worktree.display().to_string(),
        &session,
        Instant::now() + Duration::from_millis(200),
    );
    assert_eq!(code, 30, "nothing needed attention: {out}");
    assert!(out.is_empty(), "a timeout has no payload: {out}");

    // --- wait: a failed turn is exit 20 ------------------------------------
    bridge
        .send(&session, "this one the provider refuses")
        .expect("the fourth line");
    let (code, out) = tools::wait(
        &world.worktree.display().to_string(),
        &session,
        Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(code, 20, "a failed turn is exit 20: {out}");
    let failed = until(&bridge, &session, "the turn to report its failure", |v| {
        state_is(v, "failed")
    });
    assert!(
        failed["turn"]["why"]
            .as_str()
            .is_some_and(|w| !w.is_empty()),
        "a failure names why: {failed}"
    );

    // --- diff: the branch against its merge base ---------------------------
    let diff = bridge.diff(&session, false).expect("the diff");
    assert_eq!(diff["branch"], "bridge", "{diff}");
    assert_eq!(diff["base"], "main", "{diff}");
    assert!(
        diff["log"]
            .as_str()
            .unwrap_or_default()
            .contains("the branch's own work"),
        "{diff}"
    );
    assert!(
        diff["stat"]
            .as_str()
            .unwrap_or_default()
            .contains("work.txt"),
        "{diff}"
    );
    assert!(
        diff.get("diff").is_none(),
        "the full diff is asked for: {diff}"
    );
    let full = bridge.diff(&session, true).expect("the full diff");
    assert!(
        full["diff"]
            .as_str()
            .unwrap_or_default()
            .contains("+the branch's work"),
        "{full}"
    );

    // --- list, and stop ----------------------------------------------------
    let listed = bridge.list();
    let rows = listed["sessions"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["session"], session.as_str(), "{listed}");

    bridge.stop(&session).expect("the session is left");
    assert_eq!(
        bridge.list()["sessions"].as_array().map(|r| r.len()),
        Some(0),
        "a stopped session is not held"
    );
    // The session is still on disk for any head: the daemon has it and its rows are there.
    let briefs = world.registry.list();
    assert!(
        briefs
            .iter()
            .any(|b| b.session_id == session && b.status.items > 0),
        "the session and its rows outlive the bridge: {briefs:?}"
    );
}
