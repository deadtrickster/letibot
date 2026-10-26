//! **Two sessions in one daemon, and only one of them may ever read as busy** — `PROTOCOL_VERSION`
//! 52's activity, asked of the sessions a daemon actually holds.
//!
//! # The defect this pins, and what it was not
//!
//! MEASURED 2026-10-26: `crates/bridge/tests/end_to_end.rs` failed about one run in four at *"a
//! session that has never run a turn times out rather than reporting one ended"* — a `wait` on the
//! daemon's own first session answering `0 — the turn ended` after 0.8 s, about a session that had
//! run nothing at all.
//!
//! It was **not** a second session's activity leaking into the first: every session has its own
//! hub, its own `Harness`, and its own `prompt_in_flight` (`Harness::open` builds one of each), and
//! the measured log said so — the idle session's closure was called with its own hub and its own
//! flag. What was wrong was the flag itself: `turn_tail::run_turn` started the prompt's clock
//! **before the door's own act**, so a door that then found nothing to say — `Harness::wake` with
//! no notice behind it, which is what the daemon's startup ring
//! (`Sessions::put_a_restored_session_to_work`) drains on the worker's first pass — published
//! `waiting on: turn` for as long as the gathering took and `quiet` when it answered `Ok(None)`.
//! A caller watching that session armed on the first value and answered on the second, with the
//! empty message of a session that has never spoken.
//!
//! So the two halves below are one rule: **`turn` is published for a turn that runs and for
//! nothing else**, and one session's turn is never another session's answer.
//!
//! # Why `Hub::retained()` and not a sampler
//!
//! `Hub::retained()` holds every event the hub published, so a value that exists for a millisecond
//! — which is exactly what an empty wake's `turn` was — cannot be missed by looking at the wrong
//! moment. A sampler would have to be lucky, and a test that can only fail when the machine is
//! slow is a test that passes on Tuesdays.
//!
//! # What makes this runnable anywhere
//!
//! The sessions open on the **byte vocabulary** with a provider pointed at a loopback fake
//! (`vocab_gguf: None`), so no model file is needed and nothing leaves 127.0.0.1. The fake's URL
//! comes from a `providers.toml` under a scratch `XDG_CONFIG_HOME`, because that is the only door a
//! session has to a non-preset address — which means `set_var`, which is only sound in a process
//! whose other tests are not reading the environment. That is why this is a file of its own.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Dialect, Outcome, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::activity::Waiting;
use letibot_sessionlog::registry::Registry;

/// The daemon's own first session — the one `open_first` opens eagerly, and the one this test needs
/// to stay quiet.
const FIRST: &str = "s-daemon-first";
/// The second session: the daemon's same worker, its same `Parts`, its own hub.
const SECOND: &str = "s-second";

#[test]
fn a_session_that_never_turned_is_never_busy_for_anothers_turn() {
    let dir = TempDir::new("activity-two-sessions");
    let root = dir.path();
    // **Nothing of the operator's may reach these sessions**: a key in their `providers.toml`
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

    let fake = Fake::serve(vec!["the answer to the second session's prompt"]);
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/activity_two_sessions.rs — a fake on loopback.\n\
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
    cfg.session_id = FIRST.into();
    // The byte vocabulary and a provider — the door `activity_turn.rs` and `no_vocab.rs` take, so
    // this runs on a machine with no model file at all.
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
        .create(FIRST.to_string(), "", Sessions::wiring(&cfg))
        .expect("the daemon's own first session");
    registry
        .create(SECOND.to_string(), "", Sessions::wiring(&cfg))
        .expect("a second session on the same daemon");
    let mut sessions = Sessions::open_first(parts, cfg.clone(), registry.clone())
        .expect("the daemon opens its first session");
    let idle = registry.get(FIRST).expect("the first session's hub");
    let busy = registry.get(SECOND).expect("the second session's hub");
    assert!(
        idle.activity().is_quiet(),
        "a session that has run nothing says so: {:?}",
        idle.activity()
    );

    // --- the empty wake the daemon's own startup ring drains -----------------
    // `Sessions::put_a_restored_session_to_work` rings this session's wake at open, and the worker's
    // first pass is `Work::Woken` → `Sessions::wake`. On a session with nothing restored that finds
    // nothing to say — and it must therefore say nothing, above all not `turn`.
    let before = idle.retained().len();
    let outcome = sessions.wake(FIRST);
    assert!(
        matches!(outcome, Outcome::Ignored),
        "a wake with nothing behind it runs no turn"
    );
    assert_eq!(
        activity_changes(&idle, before),
        Vec::<String>::new(),
        "an empty wake is not news about what the session is waiting on"
    );
    assert!(
        idle.activity().is_quiet(),
        "and the session is still quiet: {:?}",
        idle.activity()
    );

    // --- a real turn, in the OTHER session ----------------------------------
    let reply = sessions
        .submit(SECOND, "answer this, and only this")
        .expect("the provider answers one round");
    assert_eq!(reply.rounds, 1, "one round ran, in the second session");
    assert_eq!(fake.rounds(), 1, "and the model was asked once");
    let said = activity_changes(&busy, 0);
    assert!(
        said.iter().any(|line| line.contains(Waiting::Turn.kind())),
        "the session that ran the turn says a prompt was in flight: {said:?}"
    );
    assert!(
        busy.activity().is_quiet(),
        "and once its turn is over it says so: {:?}",
        busy.activity()
    );

    // --- and the session that ran nothing is still quiet --------------------
    // The half the operator's `wait` reads: it was never handed a `turn` it could arm on, so a
    // deadline on this session is a timeout rather than the last words of somebody else's turn.
    assert_eq!(
        activity_changes(&idle, before),
        Vec::<String>::new(),
        "another session's turn is not this session's news"
    );
    assert!(
        idle.activity().is_quiet(),
        "the session that has never turned is quiet after a turn elsewhere: {:?}",
        idle.activity()
    );
}

/// Every value this hub published after `from`, as the line a head draws — `quiet` included, which
/// is the value a reader arms on and answers with.
fn activity_changes(hub: &Arc<letibot_sessionlog::Hub>, from: usize) -> Vec<String> {
    hub.retained()
        .into_iter()
        .skip(from)
        .filter_map(|env| match env.event {
            SessionEvent::ActivityChanged { activity } => Some(if activity.is_quiet() {
                "quiet".to_string()
            } else {
                activity.line()
            }),
            _ => None,
        })
        .collect()
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

/// **A fake provider on a loopback port**, answering the script in order and counting the rounds —
/// so the test can say *the model was asked*, rather than infer it from prose.
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

/// One round as SSE — the frames `letibot-provider` parses, in the shapes
/// `crates/bridge/tests/end_to_end.rs` serves (so the two fakes cannot drift on the wire bytes that
/// matter).
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

/// A duration the test never uses, kept out of the imports above on purpose: `Duration` is what a
/// wait would be written with, and this test waits for nothing — the log already holds every value.
#[allow(dead_code)]
const PATIENCE: Duration = Duration::from_secs(30);
