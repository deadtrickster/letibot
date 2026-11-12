//! A retry that works leaves nothing; a ladder that gives up leaves one row.
//!
//! The report was three rows in three seconds, one per attempt, the same sentence with a different
//! number in it - and then two more when the endpoint came back:
//!
//! ```text
//! session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:22
//! session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:23
//! session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:25
//! session · answered after 3 attempts, 12s
//! session · model_endpoint_answered · the model endpoint at api.deepseek.com answered again…
//! ```
//!
//! The operator's question is the rule: *deepseek replied on the third try - why should I focus on
//! that fact?* A ladder in flight is ONE live line, rewritten in place and withdrawn when it ends;
//! what a person is left with is one row if the ladder was spent, one row if the session moved, and
//! nothing at all if the endpoint came back. The count is not lost: `metrics.db` has it per outage
//! and on the turn's own record, which is where a question about the ladder belongs.
//!
//! A real daemon, a real socket, a real head and a loopback stub standing in for the provider -
//! nothing is mocked above the wire and nothing is injected into the retry policy, because the
//! subject is *how many rows one outage is worth*. The waits are the ladder's own - 1 s, 2 s, 4 s -
//! since a fixture that shortened them would measure a retry loop this tree does not have.
//!
//! One test and three sessions, because `XDG_CONFIG_HOME` is process-global.

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

/// The provider's own sentence, as it arrives in a 503's body - the words the row must keep
/// behind the click, because that is what gets pasted into a bug report.
const REFUSAL: &str = "the endpoint is having a bad minute";

// ---------------------------------------------------------------------------
// The stub endpoint
// ---------------------------------------------------------------------------

/// One loopback model that refuses while it is told to and answers after that. `left` is
/// the count of refusals still owed, so one stub serves every scenario in this file `seen`
/// is the evidence of how many attempts the ladder really took.
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
            // One connection at a time, so a load-then-subtract is a count. The accept loop
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

/// Drain one request - headers plus the body `Content-Length` names - so the client's write side
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

/// Everything one head was told, folded to the facts this file asserts on.
#[derive(Default)]
struct Rec {
    /// Every `model_endpoint_retry` sentence, in order - the ONE row a spent ladder is worth.
    retries: Vec<String>,
    /// Every `model_endpoint_answered` sentence. This build writes none: a recovery is not a row,
    /// and the vector is here so that a daemon that starts writing one again fails this test.
    answered: Vec<String>,
    /// Every `model_fallback` sentence - the ONE row a session moving to another model is worth.
    fallback: Vec<String>,
    /// Turns that ended FAILED.
    failed: usize,
    /// Every `ModelRetry` frame, in order - the ONE live line, as the head was told it. Three
    /// failed attempts are three frames of it: two live and one that withdraws it.
    series: Vec<Row>,
}

/// One `ModelRetry`, folded: the count, the line it was showing, the attempts behind the popup, and
/// whether the frame took the line down (`gone`). No ending text: this build never settles a line.
#[derive(Debug, Clone)]
struct Row {
    attempts: u32,
    said: String,
    log: Vec<String>,
    gone: bool,
}

fn classify(frame: ServerFrame) -> Option<Seen> {
    match frame {
        ServerFrame::Event(env) => match &env.event {
            letibot_sessionlog::event::SessionEvent::Warning { code, detail, .. } => {
                match code.as_str() {
                    "model_endpoint_retry" => Some(Seen::Retry(detail.clone())),
                    "model_endpoint_answered" => Some(Seen::Answered(detail.clone())),
                    "model_fallback" => Some(Seen::Fallback(detail.clone())),
                    _ => None,
                }
            }
            letibot_sessionlog::event::SessionEvent::TurnFailed { .. } => Some(Seen::Failed),
            letibot_sessionlog::event::SessionEvent::ModelRetry {
                attempts,
                said,
                log,
                gone,
                ..
            } => Some(Seen::Series(Row {
                attempts: *attempts,
                said: said.clone(),
                log: log.clone(),
                gone: *gone,
            })),
            _ => None,
        },
        _ => None,
    }
}

enum Seen {
    Retry(String),
    Answered(String),
    Fallback(String),
    Failed,
    Series(Row),
}

fn fold(rec: &mut Rec, inb: Inbound) {
    match classify(Inbound::frame(inb)) {
        Some(Seen::Retry(detail)) => rec.retries.push(detail),
        Some(Seen::Answered(detail)) => rec.answered.push(detail),
        Some(Seen::Fallback(detail)) => rec.fallback.push(detail),
        Some(Seen::Failed) => rec.failed += 1,
        Some(Seen::Series(row)) => rec.series.push(row),
        None => {}
    }
}

/// Pump until `done`, folding every frame in. The deadline is generous - it exists so a
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

/// How many of the stub's answers are rows yet - read off the daemon's own snapshot rather
/// than off an event tally, so this is the turn having ENDED and not a frame having arrived.
fn answers(hub: &letibot_sessionlog::hub::Hub) -> usize {
    hub.snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .filter(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains(ANSWER)))
        .count()
}

/// A config for one session of this file, on the byte vocabulary and a loopback provider -
/// so nothing here needs a GGUF, a llama.cpp or a model file of any kind.
fn config(scratch: &std::path::Path, tag: &str, retries: u32) -> Config {
    let ws = scratch.join("ws");
    std::fs::create_dir_all(&ws).expect("the workspace");
    let mut cfg = Config::for_this_box(&ws);
    // Not this box's configuration (`wired.rs`'s rule): no web search, no permission file.
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
    // No window, so no wall. The byte ledger is a large number for a prompt this small, so
    // the compaction door would fire in the middle of an outage - a second subject in a file about
    // one.
    cfg.context_window = None;
    // The ladder this file measures. `letibot_http::retry` waits 1 s, 2 s, 4 s…, so a scenario's
    // cost is the policy's and not a fixture's.
    cfg.http_retries = retries;
    // No `[fallback]` for the first two scenarios, so an exhausted ladder ends the turn rather than
    // moving the session; the third sets it, and reads the one row a move is worth.
    cfg.fallback = Vec::new();
    cfg
}

/// One session, one daemon, one head - `model_switch_at_turn_end.rs`'s arrangement: the driver
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
            // The registry is closed even if the driver panics: a panic here would otherwise
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
    let scratch = letibot_sessionlog::testing::scratch("letibot-model-outage");
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
             url = \"http://127.0.0.1:{port}/v1/chat/completions\"\n\n\
             # The name scenario C's `[fallback]` moves the session to: a fallback names another of\n\
             # the box's models, and a name with no key is refused before it is ever tried.\n\
             [glm]\nkey = \"sk-test\"\n\
             url = \"http://127.0.0.1:{port}/v1/chat/completions\"\n",
            port = endpoint.port
        ),
    )
    .expect("the config file");

    let first = config(&scratch, "probe", 2);
    // `metrics.db` is where a recovered retry is answerable at all, so the writer this daemon's CLI
    // would start is started here: beside the store, which for this test is a scratch file.
    letibot_harnessd::instruments::start_writer(&first);
    let parts = Parts::load(&first).expect("the byte vocabulary needs no model file");
    // The operator's per-project mode is not this test's business (`wired.rs`'s rule).
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // A. A ladder that runs out: three failed attempts, then the single row it is worth.
    //
    // The line moves while the ladder tries (2 attempts, then 3) and is then WITHDRAWN, and the only
    // row this session is left with is the failure that says what happened.
    endpoint.refuse(100);
    let before = endpoint.seen();
    let cfg = config(&scratch, "givesup", 2);
    let rec = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        // Waits for the withdrawn line and not only for the failed turn: without it the last frame
        // could still be in the socket, and the assertions below would read a missing ending.
        watch(rx, rec, |rec| {
            rec.failed > 0 && rec.series.last().is_some_and(|r| r.gone)
        });
    });
    let attempts = endpoint.seen() - before;
    assert_eq!(
        attempts, 3,
        "the ladder did not take the round three times, so this scenario measured nothing"
    );
    let moves: Vec<(u32, bool)> = rec.series.iter().map(|r| (r.attempts, r.gone)).collect();
    assert_eq!(
        moves,
        vec![(2, false), (3, false), (3, true)],
        "three failed attempts are ONE line that moves - 2, then 3 - and then goes: {:?}",
        rec.series
    );
    let live = &rec.series[0];
    assert!(
        live.said
            .starts_with("retrying: 2 attempts, 503 server error, ")
            && live.log.len() == 2,
        "the live line names the count and the last four words, and the popup holds one line per \
         failed attempt: {live:?}"
    );
    assert_eq!(
        rec.retries.len(),
        1,
        "three failed attempts are ONE row, and it is the one the ladder's end is worth: {:?}",
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
        rec.answered.is_empty() && rec.fallback.is_empty(),
        "an endpoint that never came back has nothing to say about answering, and the session did \
         not move: {:?} / {:?}",
        rec.answered,
        rec.fallback
    );

    // ── B. The endpoint comes back on the fourth attempt ────────────────────────────────
    //
    // The operator's question is this scenario: three failures, then an answer, and NOTHING is left on
    // the screen. The count is not lost - it is in `metrics.db` and on the turn's own record, which is
    // what the last third of this scenario reads.
    endpoint.refuse(3);
    let before = endpoint.seen();
    let cfg = config(&scratch, "comesback", 3);
    let rec = drive(&parts, cfg, |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| {
            answers(hub) >= 1 && rec.series.last().is_some_and(|r| r.gone)
        });
    });
    assert_eq!(
        endpoint.seen() - before,
        4,
        "the stub answered on the fourth attempt, so three failed and one was taken again"
    );
    assert_eq!(
        rec.series.last().map(|r| r.attempts),
        Some(3),
        "the withdrawn line carried the three that had failed, which is the count a reader watched: \
         {:?}",
        rec.series
    );
    assert!(
        rec.retries.is_empty(),
        "a recovered outage writes NO retry row: {:?}",
        rec.retries
    );
    assert!(
        rec.answered.is_empty(),
        "and no recovery row either: `deepseek replied on the third try` is not a fact to focus on: \
         {:?}",
        rec.answered
    );
    // What happened, kept where a question about it belongs: one `model.retry` row for the outage and
    // the count on the turn's own record.
    assert!(
        letibot_metrics::flush(Duration::from_secs(5)),
        "the metrics writer did not take what the daemon emitted"
    );
    assert_eq!(
        retried(&scratch, "model-outage-comesback", "answered"),
        vec![3],
        "metrics.db holds the attempts that failed, once per outage"
    );
    assert_eq!(
        turn_retries(&scratch, "model-outage-comesback"),
        Some(3),
        "and the turn's own record carries the count"
    );

    // ── C. A spent ladder the session moves away from leaves ONE row ─────────────────────
    //
    // `[fallback]` names a second model, so the ladder's end is a move rather than a failure: one row,
    // whose sentence says where the session went, and no failure row beside it.
    endpoint.refuse(3);
    let mut cfg = config(&scratch, "moves", 2);
    cfg.fallback = vec!["glm/glm-4.6".to_string()];
    let rec = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| {
            !rec.fallback.is_empty() && rec.series.last().is_some_and(|r| r.gone)
        });
    });
    assert_eq!(
        rec.fallback.len(),
        1,
        "the move is said exactly once: {:?}",
        rec.fallback
    );
    assert!(
        rec.fallback[0].contains("the session moves to glm/glm-4.6"),
        "and its sentence names where the session went: {}",
        rec.fallback[0]
    );
    assert!(
        rec.retries.is_empty(),
        "a move is not a failure and files no failure row: {:?}",
        rec.retries
    );
    assert!(
        rec.series.last().is_some_and(|r| r.gone),
        "and the line the reader was watching goes with it: {:?}",
        rec.series
    );
}

/// The failed attempts `metrics.db` recorded for one session's outages, in the order they ended - the
/// count the live line used to hold, and half the reason a recovered retry can leave no row behind.
fn retried(scratch: &std::path::Path, session: &str, outcome: &str) -> Vec<u64> {
    use letibot_metrics::store::rusqlite::params;
    let db = scratch.join("metrics.db");
    let conn =
        letibot_metrics::store::open(&db).expect("the metrics file this test's daemon wrote");
    let mut stmt = conn
        .prepare(
            "SELECT n1 FROM event WHERE kind = 'model.retry' AND session_id = ?1 AND \
             outcome = ?2 ORDER BY id",
        )
        .expect("the event table");
    stmt.query_map(params![session, outcome], |r| r.get::<_, Option<i64>>(0))
        .expect("the query")
        .filter_map(Result::ok)
        .flatten()
        .map(|n| n as u64)
        .collect()
}

/// The retry count on the turn's own record: `turn.end`'s `retries`, summed over every turn the
/// session ran, which is the number a person asking *how hard did it try* reads.
fn turn_retries(scratch: &std::path::Path, session: &str) -> Option<u64> {
    use letibot_metrics::store::rusqlite::params;
    let db = scratch.join("metrics.db");
    let conn =
        letibot_metrics::store::open(&db).expect("the metrics file this test's daemon wrote");
    conn.query_row(
        "SELECT SUM(CAST(json_extract(attrs, '$.retries') AS INTEGER)) FROM event \
         WHERE kind = 'turn.end' AND session_id = ?1",
        params![session],
        |r| r.get::<_, Option<i64>>(0),
    )
    .expect("the event table")
    .map(|n| n as u64)
}
