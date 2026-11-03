//! A stop that lands while a retry is waiting is a STOP and not just the end of the wait: the
//! session's children go first, and the failure the daemon publishes names the turn it was taken
//! in. Both were skipped when the wait learned to end early.
//!
//! ONE test in this binary, because it writes the process's config dirs before anything reads them.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::Harness;
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::CommandKind;
use letibot_sessionlog::hub::{DAEMON_SUBMITTER, Hub};
use letibot_sessionlog::registry::{Registry, SessionWiring};

/// How long anything here may take before the test calls it a hang.
const PATIENCE: Duration = Duration::from_secs(30);

/// The two prompts. The first is answered, so the session has a turn behind it.
const FIRST: &str = "say hello";
const SECOND: &str = "say hello again";

/// What the refusing stub says, so the failure can be traced to it.
const UNAVAILABLE: &str = "the endpoint is having a bad minute";

#[test]
fn a_stop_during_a_retry_wait_stops_the_children_first() {
    let dir = TempDir::new("stop-in-the-wait");
    std::fs::create_dir_all(dir.path().join("config/letibot")).expect("config dir");
    // Nothing of the operator's may reach this session: their `providers.toml` would decide where
    // a request goes, and no request may leave loopback.
    // SAFETY: this binary has ONE test, and these are set before any harness exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }
    let fake = Fake::serve();
    std::fs::write(
        dir.path().join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/stop_in_the_retry_wait.rs — a fake on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .expect("providers.toml");

    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    // A ladder long enough that the wait is still running when the stop arrives.
    cfg.http_retries = 4;
    cfg.permission = Vec::new();
    cfg.web_search = None;
    cfg.context_window = None;
    cfg.fallback = Vec::new();
    // The byte vocabulary and a provider, so this runs on a machine with no model file at all.
    cfg.vocab_gguf = None;
    cfg.session_id = "s-wait-stop".into();
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");

    let registry = Registry::new();
    let hub = registry.new_hub("s-wait-stop".to_string());
    registry
        .adopt(
            hub.clone(),
            "s-wait-stop".to_string(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: fake.url.clone(),
                workspace: workspace.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    let mut h = Harness::open_with_registry(&parts, cfg, hub.clone(), None, None, registry.clone())
        .expect("the session opens");

    // The instrument for the children: a runner that records being asked to stop, and names one
    // child so the stop has something to say. The claim is about who is told and when, not about
    // a child's own machinery.
    let stops = Arc::new(StopRecorder::default());
    h.set_subagents_for_test(stops.clone());

    // A turn of its own first: the failure this file is about is the SECOND turn's, and a session
    // whose only turn was the failed one cannot show a turn id left behind.
    h.submit(FIRST).expect("the stub answers the first turn");
    let first_turn = last_turn_started(&hub);
    assert!(
        !first_turn.is_empty(),
        "the first turn published no id, so this fixture measured nothing"
    );
    assert_eq!(
        h.last_turn_id(),
        first_turn,
        "the session remembers the turn it just ran"
    );

    // And the second: the stub refuses from here on, so the turn is in the ladder's wait when the
    // operator stops it.
    fake.refuse_from_now();
    let watching = fake.clone();
    let stopper = {
        let hub = hub.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + PATIENCE;
            while watching.seen() < 2 && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let said = hub.submit(
                DAEMON_SUBMITTER,
                "interrupt-1",
                0,
                CommandKind::Interrupt {
                    reason: "the operator stopped this turn".into(),
                },
            );
            assert!(
                matches!(said, letibot_sessionlog::ServerFrame::Accepted { .. }),
                "the hub refused the interrupt: {said:?}"
            );
        })
    };
    let out = h.submit(SECOND);
    stopper.join().expect("the thread that stopped the turn");

    // The premise: the turn really ended in a wait, and the stop is what ended it.
    let said = out
        .as_ref()
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        said.contains("stopped while the retry was waiting"),
        "the premise: the interrupt reached the retry's wait: {said}"
    );

    // The finding: the wait consumed the interrupt and nothing stopped the subtree. A stop that
    // arrives at the retry's wait IS the session being stopped, so its children go first here
    // exactly as they do at a stop the steering door takes.
    assert_eq!(
        stops.count(),
        1,
        "the children were not stopped: the wait ate the interrupt"
    );
    let warned = hub.retained().into_iter().any(|e| {
        matches!(&e.event, letibot_sessionlog::SessionEvent::Warning { code, .. }
            if code == "subagents_stopped")
    });
    assert!(
        warned,
        "the operator was told which children the stop took: no `subagents_stopped` was published"
    );
}

/// The `turn_id` of the newest `TurnStarted` on this session's log - the turn that is running.
fn last_turn_started(hub: &Arc<Hub>) -> String {
    hub.retained()
        .into_iter()
        .filter_map(|e| match e.event {
            letibot_sessionlog::SessionEvent::TurnStarted { turn_id, .. } => Some(turn_id),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}

/// A runner that starts nothing and records being asked to stop what it has.
#[derive(Default)]
struct StopRecorder {
    stops: Mutex<usize>,
}

impl StopRecorder {
    fn count(&self) -> usize {
        *self.stops.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl letibot_tools::builtins::task::TaskRunner for StopRecorder {
    fn start(
        &self,
        _prompt: &str,
        _spec: &letibot_tools::builtins::task::TaskSpec,
    ) -> Result<String, String> {
        Err("this runner starts nothing".into())
    }
    fn collect(
        &self,
        _handle: &str,
        _timeout: Duration,
    ) -> letibot_tools::builtins::task::TaskStatus {
        letibot_tools::builtins::task::TaskStatus::Unknown
    }
    fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
        *self.stops.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        vec![(
            "s-wait-stop-sub-1".into(),
            Ok("the daemon accepted the interrupt".into()),
        )]
    }
}

/// A provider on a loopback port: it answers the first request and refuses every one after it, so
/// the second turn enters the retry ladder and the ladder's wait is where the stop lands.
struct Fake {
    url: String,
    seen: Arc<AtomicUsize>,
    refuse: Arc<std::sync::atomic::AtomicBool>,
}

impl Fake {
    fn serve() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().expect("the bound port").port()
        );
        let seen: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let refusing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, r) = (seen.clone(), refusing.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut conn) = stream else { continue };
                let _ = read_request(&conn);
                s.fetch_add(1, Ordering::SeqCst);
                // One connection at a time, so the counter is the number of calls and not of
                // threads that happened to arrive together.
                let _ = match r.load(Ordering::SeqCst) {
                    true => refuse(&mut conn),
                    false => answer(&mut conn),
                };
            }
        });
        Fake {
            url,
            seen,
            refuse: refusing,
        }
    }

    fn seen(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }

    fn refuse_from_now(&self) {
        self.refuse.store(true, Ordering::SeqCst);
    }

    fn clone(&self) -> Fake {
        Fake {
            url: self.url.clone(),
            seen: self.seen.clone(),
            refuse: self.refuse.clone(),
        }
    }
}

/// `503 Service Unavailable` with the provider's own words in the body.
fn refuse(conn: &mut TcpStream) -> std::io::Result<()> {
    write!(
        conn,
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        UNAVAILABLE.len(),
        UNAVAILABLE
    )?;
    conn.flush()
}

/// One finished answer, in the three frames `letibot-provider` parses.
fn answer(conn: &mut TcpStream) -> std::io::Result<()> {
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content":" the answer"}}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":9}})
    ));
    body.push_str("data: [DONE]\n\n");
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    conn.flush()
}

/// Drain one HTTP request off the socket: headers, then the body `content-length` names.
fn read_request(stream: &TcpStream) -> String {
    use std::io::{BufRead, BufReader};
    let mut reader = BufReader::new(stream.try_clone().expect("clone the socket"));
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    let mut len = 0usize;
    loop {
        let mut head = String::new();
        if reader.read_line(&mut head).is_err() || head.trim_end().is_empty() {
            break;
        }
        if let Some(v) = head.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
