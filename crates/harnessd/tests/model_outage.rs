//! **One row per outage of one model endpoint, with a counter in it.**
//!
//! # The report
//!
//! MEASURED 2026-10-30, the operator's screen — three rows in three seconds, one per attempt,
//! the same sentence with a different number in it:
//!
//! ```text
//! session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:22
//! session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:23
//! session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:25
//! ```
//!
//! and their words: *"i dont need walls of this … a coutner is what is need"*. The daemon now
//! keeps an [`Outage`] per endpoint (`harnessd::harness`), changes it on every failed attempt and
//! **publishes nothing** until the round ends: the row then carries the count, when the last
//! attempt was, and that attempt's own sentence, and a recovery is said once.
//!
//! # What is measured here, and against what
//!
//! A real daemon, a real socket, a real head, and a loopback stub standing in for the provider —
//! nothing is mocked above the wire and nothing is injected into the retry policy, because the
//! subject is *how many rows one outage is worth* and the only honest way to count them is to let
//! a turn fail three times and read what the head was sent.
//!
//!   * **A — three failed attempts, and the ladder gives up.** `http_retries = 2`, so the round is
//!     taken three times. One `model_endpoint_retry` row, and it says `attempt 3`.
//!   * **B — a fourth attempt that answers.** One `model_endpoint_answered` row saying how many
//!     attempts it took, and no retry row at all.
//!   * **C — a second outage in the same session.** A new record and a new row: two outages are
//!     not one counter, so the second says `after 1 attempt` and not `after 4 attempts`.
//!
//! # One test, three sessions, and why not three tests
//!
//! `XDG_CONFIG_HOME` is process-global and libtest runs a binary's tests in parallel —
//! `apparatus::present`'s own tests carry the measurement of what that costs. So the scratch
//! config dir is pointed at once, by the single test that owns this process, and the three
//! scenarios run in sequence inside it. `model_switch_at_turn_end.rs` is the same shape for the
//! same reason.
//!
//! The waits are the ladder's own — 1 s, 2 s, 4 s — because the policy under it is the daemon's
//! and a fixture that shortened them would be measuring a retry loop this tree does not have.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_transcript::TranscriptItem;

/// The answer the stub streams when it is not refusing. A marker, so the row can be found.
const ANSWER: &str = "the-endpoint-came-back";

/// The prompts. Nothing tool-shaped in them: this file is about the endpoint, not the work.
const PROMPT: &str = "say hello";
const NEXT: &str = "and once more";

/// **The provider's own sentence**, as it arrives in a 503's body — the words the row must keep
/// behind the click, because that is what gets pasted into a bug report.
const REFUSAL: &str = "the endpoint is having a bad minute";

// ---------------------------------------------------------------------------
// The stub endpoint
// ---------------------------------------------------------------------------

/// One loopback model that refuses while it is told to and answers after that.
///
/// `left` is the count of refusals still owed, so one stub serves every scenario in this file:
/// a scenario sets it before it prompts, and `seen` is the evidence of how many attempts the
/// ladder really took.
#[derive(Clone)]
struct Endpoint {
    port: u16,
    left: Arc<AtomicUsize>,
    seen: Arc<AtomicUsize>,
}

impl Endpoint {
    /// How many requests this endpoint has been sent since the process started.
    fn seen(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }

    /// Refuse the next `n` requests, then answer.
    fn refuse(&self, n: usize) {
        self.left.store(n, Ordering::SeqCst);
    }
}

fn endpoint() -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    let left: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let seen: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let (l, s) = (left.clone(), seen.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut conn) = stream else { continue };
            let _body = read_request(&mut conn);
            s.fetch_add(1, Ordering::SeqCst);
            // **One connection at a time, so a load-then-subtract is a count.** The accept loop
            // is this thread; the provider sends one request and waits for its answer.
            let owed = l.load(Ordering::SeqCst);
            if owed > 0 {
                l.store(owed - 1, Ordering::SeqCst);
                let body = format!("{{\"error\":{{\"message\":\"{REFUSAL}\"}}}}");
                let _ = write!(
                    conn,
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.flush();
                continue;
            }
            let body = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{ANSWER}\"}}}}]}}\n\n\
                 data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
                 data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":4}}}}\n\n\
                 data: [DONE]\n\n"
            );
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.flush();
        }
    });
    Endpoint { port, left, seen }
}

/// Drain one request — headers plus the body `Content-Length` names — so the client's write side
/// is finished before the reply arrives.
fn read_request(s: &mut TcpStream) -> String {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
        if let Some(head_end) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
            let body_have = acc.len() - (head_end + 4);
            let body_want = acc[..head_end]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body_have >= body_want {
                return String::from_utf8_lossy(&acc[head_end + 4..head_end + 4 + body_want])
                    .into_owned();
            }
        }
        if acc.len() > (1 << 22) {
            break;
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// What one head saw
// ---------------------------------------------------------------------------

/// **Everything one head was told**, folded to the facts this file asserts on.
#[derive(Default)]
struct Rec {
    /// Every `model_endpoint_retry` sentence, in order — the row a failing outage is worth.
    retries: Vec<String>,
    /// Every `model_endpoint_answered` sentence, in order — the row a recovery is worth.
    answered: Vec<String>,
    /// Turns that ended FAILED.
    failed: usize,
}

fn classify(frame: ServerFrame) -> Option<Seen> {
    match frame {
        ServerFrame::Event(env) => match &env.event {
            letibot_sessionlog::event::SessionEvent::Warning { code, detail, .. } => {
                match code.as_str() {
                    "model_endpoint_retry" => Some(Seen::Retry(detail.clone())),
                    "model_endpoint_answered" => Some(Seen::Answered(detail.clone())),
                    _ => None,
                }
            }
            letibot_sessionlog::event::SessionEvent::TurnFailed { .. } => Some(Seen::Failed),
            _ => None,
        },
        _ => None,
    }
}

enum Seen {
    Retry(String),
    Answered(String),
    Failed,
}

fn fold(rec: &mut Rec, inb: Inbound) {
    match classify(Inbound::frame(inb)) {
        Some(Seen::Retry(detail)) => rec.retries.push(detail),
        Some(Seen::Answered(detail)) => rec.answered.push(detail),
        Some(Seen::Failed) => rec.failed += 1,
        None => {}
    }
}

/// Pump until `done`, folding every frame in. The deadline is generous — it exists so a
/// regression reads as its missing fact rather than as a hung test.
fn watch(rx: &Receiver<Inbound>, rec: &mut Rec, done: impl Fn(&Rec) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !done(rec) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(inb) => fold(rec, inb),
            Err(RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
    }
}

/// **How many of the stub's answers are rows yet** — read off the daemon's own snapshot rather
/// than off an event tally, so this is the turn having ENDED and not a frame having arrived.
fn answers(hub: &letibot_sessionlog::hub::Hub) -> usize {
    hub.snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .filter(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains(ANSWER)))
        .count()
}

/// **A config for one session of this file**, on the byte vocabulary and a loopback provider —
/// so nothing here needs a GGUF, a llama.cpp or a model file of any kind.
fn config(scratch: &std::path::Path, tag: &str, retries: u32) -> Config {
    let ws = scratch.join("ws");
    std::fs::create_dir_all(&ws).expect("the workspace");
    let mut cfg = Config::for_this_box(&ws);
    // **Not this box's configuration** (`wired.rs`'s rule): no web search, no permission file.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    // The byte vocabulary: a session whose turns go to a provider needs no GGUF.
    cfg.vocab_gguf = None;
    cfg.session_id = format!("model-outage-{tag}");
    cfg.socket = scratch.join(format!("{tag}.sock"));
    cfg.store = Some(scratch.join(format!("{tag}.db")));
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".to_string(),
        model: Some("deepseek-chat".to_string()),
        api_key: None,
        thinking: false,
    });
    // **No window, so no wall.** The ledger is byte tokens here (one id per byte of the rendered
    // text), which is a large number for a prompt this small and would have the compaction door
    // firing in the middle of an outage — a second subject in a file about one.
    cfg.context_window = None;
    // The ladder this file measures. `http_retry_after` waits 1 s, 2 s, 4 s…, so a scenario's
    // cost is the policy's and not a fixture's.
    cfg.http_retries = retries;
    // No `[fallback]` in the providers.toml this file writes, so an exhausted ladder ends the
    // turn rather than moving the session — which is the case A is about.
    cfg.fallback = Vec::new();
    cfg
}

/// **One session, one daemon, one head** — `model_switch_at_turn_end.rs`'s arrangement: the driver
/// on a thread of its own and the worker loop on this one.
fn drive<F>(parts: &Parts, cfg: Config, body: F) -> Rec
where
    F: FnOnce(&mut HeadClient, &Receiver<Inbound>, &letibot_sessionlog::hub::Hub, &mut Rec)
        + Send
        + 'static,
{
    let session = cfg.session_id.clone();
    let registry = Registry::new();
    registry
        .create(session.clone(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let daemon = Daemon::serve(registry.clone(), &cfg.socket).expect("the socket binds");
    let (client, _hello, reader) = HeadClient::attach(
        &cfg.socket,
        &session,
        0,
        "tui",
        "model-outage-test",
        Caps::default(),
    )
    .expect("a head attaches");
    let (tx, rx) = channel();
    std::thread::spawn(move || pump(reader, tx));
    let hub = registry
        .get(&session)
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("the session opens");

    let driver = {
        let registry = registry.clone();
        let hub = hub.clone();
        std::thread::spawn(move || {
            let mut client = client;
            let mut rec = Rec::default();
            // **The registry is closed even if the driver panics**: a panic here would otherwise
            // leave `daemon.run` blocked on a bell nobody rings, and a regression would read as a
            // hung test rather than as the missing fact it is.
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                body(&mut client, &rx, &hub, &mut rec);
            }));
            registry.close();
            if let Err(why) = ran {
                std::panic::resume_unwind(why);
            }
            rec
        })
    };

    daemon.run(&mut sessions, |_, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();
    rec
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// The whole of the rule, measured once, in one process.
#[test]
fn one_outage_is_one_row_with_a_counter() {
    // The operator's `providers.toml` must not be touched: point the config home at a scratch
    // directory for this process, and take the keys this file writes out of anybody else's reach.
    let scratch = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-model-outage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: this test binary has one test, and everything below runs in sequence on this thread
    // or on threads it starts afterwards; nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    let endpoint = endpoint();
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        format!(
            "# Written by model_outage.rs. Nothing here is the operator's file.\n\
             [deepseek]\nkey = \"sk-test\"\n\
             url = \"http://127.0.0.1:{}/v1/chat/completions\"\n",
            endpoint.port
        ),
    )
    .expect("the config file");

    let first = config(&scratch, "probe", 2);
    let parts = Parts::load(&first).expect("the byte vocabulary needs no model file");
    // The operator's per-project mode is not this test's business (`wired.rs`'s rule).
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // ── A. Three failed attempts, and the ladder gives up ────────────────────────────────
    //
    // One row, and it carries the count: the operator's wall of three was the defect.
    endpoint.refuse(100);
    let before = endpoint.seen();
    let cfg = config(&scratch, "givesup", 2);
    let rec = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| rec.failed > 0);
    });
    let attempts = endpoint.seen() - before;
    assert_eq!(
        attempts, 3,
        "the ladder did not take the round three times, so this scenario measured nothing"
    );
    assert_eq!(
        rec.retries.len(),
        1,
        "three failed attempts are ONE row, not one per attempt: {:?}",
        rec.retries
    );
    let row = &rec.retries[0];
    assert!(
        row.contains("did not answer") && row.contains("(attempt 3, last at "),
        "the row must carry the count and when the last attempt was: {row}"
    );
    assert!(
        row.contains(REFUSAL),
        "and the provider's own sentence, which is what a bug report gets: {row}"
    );
    assert!(
        rec.answered.is_empty(),
        "an endpoint that never came back has nothing to say about answering: {:?}",
        rec.answered
    );

    // ── B. A fourth attempt that answers ────────────────────────────────────────────────
    //
    // The recovery is said once, with the count — a silent one is how somebody spends an
    // afternoon believing the endpoint is still down — and no retry row is written at all.
    endpoint.refuse(3);
    let before = endpoint.seen();
    let cfg = config(&scratch, "comesback", 3);
    let rec = drive(&parts, cfg, |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |_| answers(hub) >= 1);
    });
    assert_eq!(
        endpoint.seen() - before,
        4,
        "the stub answered on the fourth attempt, so three failed and one was taken again"
    );
    assert!(
        rec.retries.is_empty(),
        "a recovered outage writes no retry row: {:?}",
        rec.retries
    );
    assert_eq!(
        rec.answered.len(),
        1,
        "the recovery is said exactly once: {:?}",
        rec.answered
    );
    let said = &rec.answered[0];
    assert!(
        said.contains("answered again after 3 attempts / "),
        "the recovery names how many attempts it took: {said}"
    );
    assert!(
        said.contains(REFUSAL),
        "and keeps the last thing the endpoint said, for the click: {said}"
    );

    // ── C. A second outage in the same session is a NEW row ─────────────────────────────
    //
    // Two outages are not one counter: the second says `after 1 attempt`, not `after 4`.
    endpoint.refuse(1);
    let cfg = config(&scratch, "second", 2);
    let ep = endpoint.clone();
    let rec = drive(&parts, cfg, move |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |_| answers(hub) >= 1);
        // The second outage, in the same session and against the same endpoint.
        ep.refuse(1);
        client
            .prompt(0, NEXT)
            .expect("the second prompt is accepted");
        watch(rx, rec, |_| answers(hub) >= 2);
    });
    assert_eq!(
        rec.answered.len(),
        2,
        "each outage earns its own row: {:?}",
        rec.answered
    );
    assert!(
        rec.answered[0].contains("after 1 attempt /"),
        "the first outage was one failed attempt: {:?}",
        rec.answered[0]
    );
    assert!(
        rec.answered[1].contains("after 1 attempt /"),
        "and the second starts its own count rather than continuing the first: {:?}",
        rec.answered[1]
    );
    assert!(
        rec.retries.is_empty(),
        "neither outage was one the ladder gave up on: {:?}",
        rec.retries
    );
}
