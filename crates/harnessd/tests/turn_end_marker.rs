//! A round's end and a turn's end are told apart on the wire. A turn of two rounds publishes two
//! `TurnFinished` frames, and exactly one of them says the PROMPT is over.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::Harness;
use letibot_harnessd::turn_tail::{Bare, Door, run_turn};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::registry::{Registry, SessionWiring};

#[test]
fn a_two_round_turn_ends_once_and_says_which_frame_ended_it() {
    let dir = TempDir::new("turn-end-marker");
    let root = dir.path();
    // Nothing of the operator's may reach this session: a key in their `providers.toml` would decide
    // where a request goes, and no request may leave loopback.
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    // SAFETY: this binary has ONE test, and these are set before any thread that reads config exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }

    let target = root.join("plan.md");
    std::fs::write(&target, "# the plan\n").expect("the file the call reads");
    let fake = Fake::serve(target.display().to_string());
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/turn_end_marker.rs: a fake on loopback.\n\
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
    cfg.session_id = "s-turn-end-marker".into();
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
    let mut h = Harness::open_with_registry(&parts, cfg.clone(), hub.clone(), None, None, registry)
        .expect("the session opens");

    let before = hub.retained().len();
    run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
        h.submit("read the plan and answer").map(Some)
    })
    .expect("a spoken turn always runs")
    .expect("the turn runs");
    assert_eq!(fake.rounds(), 2, "a tool round and then the answer");

    // The frames as the log holds them, read back through the wire: `ends_turn` is the field a head
    // asks, and it has to be there in the daemon's own spelling rather than in a local variable.
    let finished: Vec<serde_json::Value> = hub
        .retained()
        .into_iter()
        .skip(before)
        .filter(|env| matches!(env.event, SessionEvent::TurnFinished { .. }))
        .map(|env| serde_json::to_value(&env.event).expect("an event is serialisable"))
        .collect();
    assert_eq!(
        finished.len(),
        2,
        "one finish per round, and no second ending: {finished:#?}"
    );
    assert_eq!(
        finished[0].get("ends_turn"),
        Some(&serde_json::Value::Bool(false)),
        "the round that proposed a call is not the turn's end: {:#?}",
        finished[0]
    );
    assert_eq!(
        finished[1].get("ends_turn"),
        Some(&serde_json::Value::Bool(true)),
        "the round with no calls left to run is: {:#?}",
        finished[1]
    );
    // Both rounds are one turn's, and the round's own metrics are still its own: the ending is a
    // fact about the round that carried it, not a second publication of it.
    assert_ne!(
        finished[0]["turn_id"], finished[1]["turn_id"],
        "the two rounds are two turns of the engine: {finished:#?}"
    );
    for f in &finished {
        assert!(
            f["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 0,
            "a round's own figures travel with it: {f:#?}"
        );
    }
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

/// A fake provider on a loopback port: one `read` call, then the answer, counting its rounds so the
/// test can say a tool round happened rather than inferring it from prose.
struct Fake {
    url: String,
    rounds: Arc<Mutex<usize>>,
}

impl Fake {
    fn serve(path: String) -> Fake {
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
                let Ok(conn) = conn else { continue };
                // The request is drained and discarded: what this fake answers is the script, and a
                // provider that answered from the conversation would be a second model.
                let _ = read_request(&conn);
                turn += 1;
                *seen.lock().unwrap_or_else(|e| e.into_inner()) = turn;
                let _ = write_sse(conn, &path, turn);
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

/// One round as SSE, in the shapes `crates/bridge/tests/end_to_end.rs` serves, so two fakes cannot
/// drift on the wire bytes.
fn write_sse(mut stream: std::net::TcpStream, path: &str, round: usize) -> std::io::Result<()> {
    use std::io::Write;
    let mut body = String::new();
    if round == 1 {
        let arguments = serde_json::json!({ "path": path }).to_string();
        body.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{
                "index": 0,
                "id": "call_turn_end_1",
                "function": { "name": "read", "arguments": arguments },
            }]}}]})
        ));
        body.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})
        ));
    } else {
        body.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{"content":"read it"}}]})
        ));
        body.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
        ));
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
