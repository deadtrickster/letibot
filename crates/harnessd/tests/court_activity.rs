//! A command that starts no turn leaves the session quiet: the session's own thread takes it, and
//! a head must not go on showing `delivery` for work nobody owes.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::Registry;

const ONLY: &str = "s-court-only";

#[test]
fn a_command_that_starts_no_turn_leaves_the_session_quiet() {
    let dir = TempDir::new("court-activity");
    let root = dir.path();
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
    let fake = Fake::serve(vec!["unused"]);
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/court_activity.rs - a fake on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .expect("providers.toml");

    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.session_id = ONLY.into();
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts: &'static Parts = Box::leak(Box::new(
        Parts::load(&cfg).expect("the byte vocabulary needs no model file"),
    ));
    let registry = Registry::new();
    registry
        .create(ONLY.to_string(), "", Sessions::wiring(&cfg))
        .expect("the session");
    let mut sessions =
        Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("the daemon opens it");
    sessions.serve_each_on_its_own_thread(Arc::new(|_, _| {}));
    let hub = registry.get(ONLY).expect("the session's hub");

    // Through a head, as the operator's head sends it: `submit` publishes `delivery` at once.
    let head = hub.attach("tui", "dead@box", Caps::default(), 0);
    let seq = hub.retained().last().map(|e| e.seq).unwrap_or(0);
    // A command whose answer is a frame to one head, not a row: nothing it does republishes.
    let _ = hub.submit(&head.head_id, "r-1", seq, CommandKind::WithdrawPrompts);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && (hub.work_pending() || !hub.activity().is_quiet()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let said: Vec<String> = hub
        .retained()
        .into_iter()
        .filter_map(|e| match e.event {
            letibot_sessionlog::SessionEvent::ActivityChanged { activity } => {
                Some(format!("{activity:?}"))
            }
            _ => None,
        })
        .collect();
    assert!(!hub.work_pending(), "the session's thread took the command");
    assert!(
        hub.activity().is_quiet(),
        "a taken command that ran no turn is still published: {said:?}"
    );
    assert_eq!(fake.rounds(), 0, "and no model was asked");
    drop(sessions);
}

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch(tag);
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

/// A fake provider on a loopback port, answering the script in order and counting the rounds, so
/// the test can say the model was asked rather than infer it from prose.
struct Fake {
    url: String,
    rounds: Arc<Mutex<usize>>,
}

impl Fake {
    fn serve(script: Vec<&'static str>) -> Fake {
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
                let text = script.get(turn).copied().unwrap_or("done after the script");
                turn += 1;
                *seen.lock().unwrap_or_else(|e| e.into_inner()) = turn;
                let _ = write_sse(conn, text);
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

/// One round as SSE, in the shapes `crates/bridge/tests/end_to_end.rs` serves, so the two fakes
/// cannot drift on the wire bytes that matter.
fn write_sse(mut stream: std::net::TcpStream, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content": text}}]})
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

/// Unused on purpose: `Duration` is what a wait would be written with, and this test waits for
/// nothing - the log already holds every value.
#[allow(dead_code)]
const PATIENCE: Duration = Duration::from_secs(30);
