//! What the model is told about memory where it chooses a call's cap: a line in the round's own
//! tail, saying what is free. Not the schema - a schema is the session's stable prefix, and the
//! numbers move every second - so the second round's tools are byte-identical to the first's and
//! the line is there exactly once.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::registry::{Registry, SessionWiring};

#[test]
fn every_round_says_what_is_free_and_no_schema_carries_it() {
    let dir = TempDir::new("memory-line");
    let root = dir.path();
    // Nothing of the operator's may reach this session: a key in their `providers.toml` would decide
    // where a request goes, and no request may leave loopback.
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
            "# Written by crates/harnessd/tests/memory_line.rs — a fake on loopback.\n\
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
    cfg.session_id = "s-memory-line".into();
    // The byte vocabulary and a provider, so this runs on a machine with no model file at all.
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

    use letibot_harnessd::turn_tail::{Bare, Door, run_turn};
    run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
        h.submit("read the plan and answer").map(Some)
    })
    .expect("a spoken turn always runs")
    .expect("the turn runs");

    let sent = fake.sent();
    assert_eq!(sent.len(), 2, "a tool round and then the answer");
    let bodies: Vec<serde_json::Value> = sent
        .iter()
        .map(|b| serde_json::from_str(b).expect("a json request"))
        .collect();
    for (n, body) in bodies.iter().enumerate() {
        let text = body.to_string();
        assert!(
            text.contains("[memory - the harness's own line"),
            "round {} never told the model what is free: {text}",
            n + 1
        );
        assert!(
            text.contains("is free, swap") && text.contains("an ask up to"),
            "the line carries the box's numbers: {text}"
        );
        assert_eq!(
            text.matches("[memory - the harness's own line").count(),
            1,
            "round {} carries one line, not one per round behind it: {text}",
            n + 1
        );
        // The schema is the stable prefix, so it must not carry a number that moves.
        let tools = body["tools"].to_string();
        assert!(
            !tools.contains("is free, swap"),
            "the tools are the stable prefix and cannot carry a live number"
        );
    }
    assert_eq!(
        bodies[0]["tools"], bodies[1]["tools"],
        "a line that moved the schema would re-prefill every warm conversation"
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
    /// One `read` call - a tool the gate admits without a card, so the turn goes on to a second round
    /// with nobody to ask.
    Read { path: String },
}

/// A fake provider on a loopback port, answering the scripted rounds in order and keeping every
/// request body, so the test reads what the model was actually sent.
struct Fake {
    url: String,
    sent: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn serve(script: Vec<Round>) -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let sent = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&sent);
        std::thread::spawn(move || {
            let mut turn = 0usize;
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let body = read_request(&conn);
                seen.lock().unwrap_or_else(|e| e.into_inner()).push(body);
                let round = script
                    .get(turn)
                    .cloned()
                    .unwrap_or(Round::Say("done after the script"));
                turn += 1;
                let _ = write_sse(conn, &round);
            }
        });
        Fake { url, sent }
    }

    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
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

/// One round as SSE - the three frames `letibot-provider` parses, in the shapes
/// `crates/harnessd/tests/activity_turn.rs` serves, so the two fakes cannot drift on the wire bytes.
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
                    "id": "call_memory_1",
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
