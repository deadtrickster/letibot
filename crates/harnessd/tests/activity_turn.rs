//! **A turn with two rounds, and the whole published history of what the session was waiting on.**
//!
//! `PROTOCOL_VERSION` 52's `Turn` is the PROMPT and not a round: the engine publishes
//! `TurnStarted`/`TurnFinished` once per round, and a round that ended with tool calls is *always*
//! followed by another round of the same prompt — so a `Turn` derived from that pair reads *quiet*
//! at every round boundary, which is a session reported **done** in the middle of its own work. It
//! is the last false wake of the package (the bridge's `wait` exited there, QC's `wait` exited
//! there, and R55's row stood down there) and the reason `Waiting::Turn` is read from
//! `Harness::prompt_in_flight` — the span `begin_turn_clock` brackets, *"called once per prompt,
//! never per round"* — with the hub's round-level state only as the fallback for a hub nobody runs
//! turns on.
//!
//! # What makes this runnable anywhere
//!
//! The session opens on the **byte vocabulary** with a provider (`vocab_gguf: None`,
//! `provider: Some(..)`), so no model file is needed; the provider is a loopback fake scripting
//! two rounds — a `read` call, which the gate admits without a card, and then the answer. The
//! provider's URL comes from a `providers.toml` under a scratch `XDG_CONFIG_HOME`, because that is
//! the only door a session has to a non-preset address, and this test therefore owns its whole
//! process: `set_var` around a config read is only sound when nothing else in the binary reads the
//! environment. That is why it is a file of its own.
//!
//! # Why the assertion is read off the log and not off a sampler
//!
//! `Hub::retained()` holds every event the hub published, so **no value can be missed between two
//! samples**. A sampler would have to be lucky enough to look inside the round boundary, which is
//! milliseconds wide, and a test that can only fail when the machine is slow is a test that passes
//! on Tuesdays.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::registry::{Registry, SessionWiring};

#[test]
fn a_two_round_turn_is_never_quiet_between_its_rounds() {
    let dir = TempDir::new("activity-turn");
    let root = dir.path();
    // **Nothing of the operator's may reach this session**: a key in their `providers.toml`
    // would decide where a request goes, and this test's one rule is that no request leaves
    // loopback.
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    // SAFETY: this binary has ONE test, and these are set before the daemon, any thread that reads
    // config, or the harness exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }

    let target = root.join("plan.md");
    std::fs::write(&target, "# the plan\n").expect("the file the call reads");
    let fake = Fake::serve(vec![
        Round::Read {
            path: target.display().to_string(),
        },
        Round::Say("read it"),
    ]);
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/activity_turn.rs — a fake on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .expect("providers.toml");

    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.session_id = "s-activity-turn".into();
    // **The byte vocabulary and a provider** — the door `idle_clock.rs` and `no_vocab.rs` take, so
    // this runs on a machine with no model file at all.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");

    let registry = Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    registry
        .adopt(
            hub.clone(),
            cfg.session_id.clone(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: fake.url.clone(),
                workspace: workspace.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    let mut h = Harness::open_with_registry(
        &parts,
        cfg.clone(),
        hub.clone(),
        None,
        None,
        registry.clone(),
    )
    .expect("the session opens");
    assert!(
        hub.activity().is_quiet(),
        "an idle session says so: {:?}",
        hub.activity()
    );

    let before = hub.retained().len();
    // **The door the daemon uses**, not `Harness::submit` on its own: a prompt's span is what
    // `turn_tail::run_turn` brackets (`begin_turn_clock`/`end_turn_clock`), and every production
    // door — `Sessions::run_prompt`, `Sessions::wake`, a child's own thread — goes through it. A
    // test that called `submit` directly would be testing the half inside the span, which is the
    // half that reads *quiet* at a round boundary.
    use letibot_harnessd::turn_tail::{Bare, Door, run_turn};
    run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
        h.submit("read the plan and answer").map(Some)
    })
    .expect("a spoken turn always runs")
    .expect("the turn runs");
    assert_eq!(
        fake.rounds(),
        2,
        "a tool round and then the answer — the boundary this test is about"
    );

    let published: Vec<Activity> = hub
        .retained()
        .into_iter()
        .skip(before)
        .filter_map(|env| match env.event {
            SessionEvent::ActivityChanged { activity } => Some(activity),
            _ => None,
        })
        .collect();
    let said: Vec<String> = published
        .iter()
        .map(|a| {
            if a.is_quiet() {
                "quiet".to_string()
            } else {
                a.line()
            }
        })
        .collect();
    assert!(
        !published.is_empty(),
        "the daemon says something while a turn runs: {said:?}"
    );
    assert!(
        published
            .iter()
            .any(|a| { a.waiting_on().contains(&Waiting::Turn) }),
        "a prompt makes the session busy, and the value says which kind: {said:?}"
    );
    // **Exactly one quiet, and it is the last one.** Anything else is a session reported *done* in
    // the middle of its own prompt — the window between round one's last call finishing and round
    // two's `TurnStarted`, which the round-level pair cannot help with and which this test would
    // fail on before `Turn` became the prompt's span.
    let quiets: Vec<usize> = published
        .iter()
        .enumerate()
        .filter_map(|(i, a)| a.is_quiet().then_some(i))
        .collect();
    assert_eq!(
        quiets,
        vec![published.len() - 1],
        "the session went quiet in the middle of its own turn: {said:?}"
    );
    assert!(
        hub.activity().is_quiet(),
        "and once nothing is left it says so: {:?}",
        hub.activity()
    );
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
                .duration_since(std::time::UNIX_EPOCH)
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

/// One round, in the provider's wire shape.
#[derive(Clone)]
enum Round {
    /// Prose and a normal stop.
    Say(&'static str),
    /// One `read` call — a tool the gate admits without a card, so the turn goes on to a second
    /// round with nobody to ask.
    Read { path: String },
}

/// **A fake provider on a loopback port**, answering the scripted rounds in order and counting them
/// — so the test can say *a tool round and then the answer happened*, rather than inferring it from
/// prose.
struct Fake {
    url: String,
    rounds: Arc<Mutex<usize>>,
}

impl Fake {
    fn serve(script: Vec<Round>) -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let rounds = Arc::new(Mutex::new(0usize));
        let seen = Arc::clone(&rounds);
        std::thread::spawn(move || {
            let mut turn = 0usize;
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                // The request is drained and discarded: what this fake answers is the script, and a
                // provider that answered from the conversation would be a second model.
                let _ = read_request(&conn);
                let round = script
                    .get(turn)
                    .cloned()
                    .unwrap_or(Round::Say("done after the script"));
                turn += 1;
                *seen.lock().unwrap_or_else(|e| e.into_inner()) = turn;
                let _ = write_sse(conn, &round);
            }
        });
        Fake { url, rounds }
    }

    fn rounds(&self) -> usize {
        *self.rounds.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Drain one HTTP request off the socket: headers, then the body `content-length` names.
fn read_request(stream: &std::net::TcpStream) -> String {
    use std::io::{BufRead, BufReader, Read};
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

/// One round as SSE — the three frames `letibot-provider` parses, in the shapes
/// `crates/bridge/tests/end_to_end.rs` serves (so the two fakes cannot drift on the wire bytes that
/// matter).
fn write_sse(mut stream: std::net::TcpStream, round: &Round) -> std::io::Result<()> {
    use std::io::Write;
    let mut body = String::new();
    match round {
        Round::Say(text) => {
            body.push_str(&format!(
                "data: {}\n\n",
                serde_json::json!({"choices":[{"delta":{"content": text}}]})
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
            ));
        }
        Round::Read { path } => {
            let arguments = serde_json::json!({ "path": path }).to_string();
            body.push_str(&format!(
                "data: {}\n\n",
                serde_json::json!({"choices":[{"delta":{"tool_calls":[{
                    "index": 0,
                    "id": "call_activity_1",
                    "function": { "name": "read", "arguments": arguments },
                }]}}]})
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})
            ));
        }
    }
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[],"usage":{
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

/// A duration the test never uses, kept out of the imports above on purpose: `Duration` is what a
/// wait would be written with, and this test waits for nothing — the log already holds every value.
#[allow(dead_code)]
const PATIENCE: Duration = Duration::from_secs(30);
