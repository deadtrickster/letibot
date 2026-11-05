//! The prompt's ending on the wire when the turn does not end the way a plain answer does: a round
//! with no call that a nudge continues, and a loop that returns between rounds. In both, the hub
//! says in flight until the one frame with `ends_turn`, and that frame is published exactly once.

use std::time::SystemTime;

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::{Harness, Reply};
use letibot_harnessd::turn_tail::{Bare, Door, run_turn};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};

const EXCUSE: &str = "My context is at the wall, so I'll stop here rather than start a piece I can't finish: what's left of the port is scroll regions.";

fn endings(hub: &Hub, from: usize) -> Vec<bool> {
    hub.retained()
        .into_iter()
        .skip(from)
        .filter_map(|e| match e.event {
            SessionEvent::TurnFinished { ends_turn, .. } => Some(ends_turn),
            _ => None,
        })
        .collect()
}

#[test]
fn a_nudged_round_and_a_loop_left_between_rounds_both_end_the_prompt_once() {
    let dir = TempDir::new("turn-end-nudge");
    let root = dir.path();
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    // SAFETY: this binary has ONE test, and these are set before any thread that reads config exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }
    let fake = Fake::serve(vec![EXCUSE, "Continuing with the scroll regions."]);
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/turn_end_nudge.rs: a fake on loopback.\n\
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
    cfg.session_id = "s-turn-end-nudge".into();
    cfg.vocab_gguf = None;
    cfg.context_window = Some(1_000_000);
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

    // A round with no call, nudged into a second one: the first round does not end the prompt.
    let before = hub.retained().len();
    let reply = run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
        h.submit("port the rest of the emulator").map(Some)
    })
    .expect("a spoken turn always runs")
    .expect("the turn runs");
    assert_eq!(
        reply.rounds, 2,
        "the premise: the excuse was nudged: {reply:?}"
    );
    assert_eq!(endings(&hub, before), vec![false, true]);
    assert!(!hub.turn_in_flight(), "the ending stood the turn down");

    // A loop that returns after a round that said the prompt goes on: the tail ends it, once.
    let before = hub.retained().len();
    run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
        let hub = h.hub().clone();
        hub.publish(letibot_sessionlog::testing::turn_started("t-left"));
        hub.publish(letibot_sessionlog::testing::turn_finished_round("t-left"));
        assert!(hub.turn_in_flight(), "the premise: a round said it goes on");
        Ok(Some(Reply {
            text: String::new(),
            metrics: Vec::new(),
            rounds: 1,
            tool_calls: 1,
            truncated: false,
        }))
    })
    .expect("a spoken turn always runs")
    .expect("the turn runs");
    assert_eq!(endings(&hub, before), vec![false, true]);
    assert!(!hub.turn_in_flight());
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

/// A fake provider on a loopback port that answers each request with the next script line as a plain
/// text round, so a turn can run without a model file.
struct Fake {
    url: String,
}

impl Fake {
    fn serve(script: Vec<&'static str>) -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        std::thread::spawn(move || {
            let mut turn = 0usize;
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let _ = read_request(&conn);
                let said = script[turn.min(script.len() - 1)];
                turn += 1;
                let _ = write_sse(conn, said);
            }
        });
        Fake { url }
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

/// One plain-text round as SSE, in the shapes `turn_end_marker.rs` serves.
fn write_sse(mut stream: std::net::TcpStream, said: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content":said}}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
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
