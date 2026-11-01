//! **What is retryable, and for how long** — measured through a real daemon, a real head and a
//! loopback stub, because the two reports this answers were both about *how many times the
//! provider was asked* and nothing short of counting the stub's connections can say.
//!
//! # The two reports, and why they are one rule
//!
//! * MEASURED, a child's turn ended with `FAILED — http io: provider unreachable: io: failed to
//!   lookup address information: Temporary failure in name resolution (nothing was recorded)`.
//!   A DNS blip is the textbook *temporary* error, and the sentence does not say whether the
//!   daemon tried once or six times.
//! * Earlier, a `429` was retried **six times** before the operator saw it — a permanent refusal
//!   presented as a transient one.
//!
//! Both are the same missing decision, and it is [`letibot_http::retry`]'s: three classes, one
//! bounded ladder, and a sentence for each ending. What this file measures is the part only a
//! wire can: **how many calls the provider actually got**, and what the operator was told.
//!
//!   * **A — a transport failure is retried, and the turn survives.** The stub accepts and hangs
//!     up on the first call. Two calls, one answer, no failure: a turn that never reached the
//!     network is tried before it is called a failure.
//!   * **B — a `503` is retried** for the same reason, and answers on the second call.
//!   * **C — a `401` is not retried.** One call, and the sentence is the status and the body.
//!   * **D — a `429` that names money is not retried.** One call. This is the six-times case:
//!     the test that would have seen six calls now sees one.
//!   * **E — the ladder runs out, and says so**: `tried 2 times over 1s, then gave up`.
//!
//! # One test, five sessions, and why not five tests
//!
//! `XDG_CONFIG_HOME` is process-global and libtest runs a binary's tests in parallel —
//! `apparatus::present`'s own tests carry the measurement of what that costs. So the scratch
//! config dir is pointed at once, by the single test that owns this process, and the five
//! scenarios run in sequence inside it. `model_outage.rs` is the same shape for the same reason.

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
const ANSWER: &str = "the-provider-answered";

/// The prompt. Nothing tool-shaped in it: this file is about the endpoint, not the work.
const PROMPT: &str = "say hello";

/// The provider's own sentence in a `503`'s body — the words a sentence must keep behind the
/// click, because that is what gets pasted into a bug report.
const UNAVAILABLE: &str = "the endpoint is having a bad minute";

/// What a `429` that names money says, in OpenAI's own shape — the six-times case.
const NO_CREDIT: &str =
    r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#;

// ---------------------------------------------------------------------------
// The stub endpoint
// ---------------------------------------------------------------------------

/// **What the stub does to a call**, chosen per scenario.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// Accept the connection and hang up without a byte of answer — a transport failure, which
    /// is the class a DNS lookup failure and a refused connection both land in.
    HangUp = 0,
    /// `503 Service Unavailable`, the status a server reloading a model answers.
    Unavailable = 1,
    /// `401 Unauthorized` — a key the provider refused.
    Unauthorized = 2,
    /// `429` whose body says the account cannot pay.
    NoCredit = 3,
}

impl Refusal {
    fn of(n: usize) -> Refusal {
        match n {
            1 => Refusal::Unavailable,
            2 => Refusal::Unauthorized,
            3 => Refusal::NoCredit,
            _ => Refusal::HangUp,
        }
    }
}

#[derive(Clone)]
struct Endpoint {
    port: u16,
    /// How many refusals are still owed, and what kind.
    left: Arc<AtomicUsize>,
    how: Arc<AtomicUsize>,
    /// How many calls this endpoint has been sent since the process started.
    seen: Arc<AtomicUsize>,
}

impl Endpoint {
    fn seen(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }

    /// Refuse the next `n` calls in `how`, then answer.
    fn refuse(&self, n: usize, how: Refusal) {
        self.how.store(how as usize, Ordering::SeqCst);
        self.left.store(n, Ordering::SeqCst);
    }
}

fn endpoint() -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    let left: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let how: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let seen: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let (l, h, s) = (left.clone(), how.clone(), seen.clone());
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
                match Refusal::of(h.load(Ordering::SeqCst)) {
                    // Not a status at all: the connection goes away with nothing written, which
                    // is what a resolver that could not answer, or a server that is restarting,
                    // looks like from here.
                    Refusal::HangUp => {
                        let _ = conn.shutdown(std::net::Shutdown::Both);
                        continue;
                    }
                    Refusal::Unavailable => {
                        answer(&mut conn, 503, "Service Unavailable", UNAVAILABLE)
                    }
                    Refusal::Unauthorized => {
                        answer(&mut conn, 401, "Unauthorized", "invalid api key")
                    }
                    Refusal::NoCredit => answer(&mut conn, 429, "Too Many Requests", NO_CREDIT),
                }
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
    Endpoint {
        port,
        left,
        how,
        seen,
    }
}

/// One non-2xx answer, with the body the provider's own words live in.
fn answer(conn: &mut TcpStream, code: u16, why: &str, body: &str) {
    let _ = write!(
        conn,
        "HTTP/1.1 {code} {why}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = conn.flush();
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
    /// Every `turn_failed` sentence, in order — what the operator read under `FAILED — `.
    failed: Vec<String>,
}

fn fold(rec: &mut Rec, inb: Inbound) {
    if let ServerFrame::Event(env) = Inbound::frame(inb)
        && let letibot_sessionlog::event::SessionEvent::TurnFailed { error, .. } = &env.event
    {
        rec.failed.push(error.clone());
    }
}

/// Pump until `done`, folding every frame in. The deadline is generous — it exists so a
/// regression reads as its missing fact rather than as a hung test.
fn watch(rx: &Receiver<Inbound>, rec: &mut Rec, done: impl Fn(&Rec) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
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
    cfg.session_id = format!("retry-policy-{tag}");
    cfg.socket = scratch.join(format!("{tag}.sock"));
    cfg.store = Some(scratch.join(format!("{tag}.db")));
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".to_string(),
        model: Some("deepseek-chat".to_string()),
        // **A key the session may not replace.** The 401 arm asks the operator for a new key
        // before the ladder is consulted, and a card nobody can answer is a second subject in a
        // file about the retry rule.
        api_key: Some("sk-test".to_string()),
        thinking: false,
    });
    // **No window, so no wall.** The ledger is byte tokens here (one id per byte of the rendered
    // text), a large number for a prompt this small, and a compaction door firing in the middle
    // of an outage would be a second subject.
    cfg.context_window = None;
    // The ladder this file measures: the policy's own waits, so a scenario's cost is the
    // policy's and not a fixture's.
    cfg.http_retries = retries;
    // No `[fallback]`, so an exhausted ladder ends the turn rather than moving the session.
    cfg.fallback = Vec::new();
    cfg
}

/// **One session, one daemon, one head** — `model_outage.rs`'s arrangement: the driver on a
/// thread of its own and the worker loop on this one.
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
        "retry-policy-test",
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
            // leave `daemon.run` blocked on a bell nobody rings, and a regression would read as
            // a hung test rather than as the missing fact it is.
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

/// **The whole of the rule, measured once, in one process** — five scenarios, five sessions.
#[test]
fn what_is_retryable_and_what_the_operator_is_told() {
    // The operator's `providers.toml` must not be touched: point the config home at a scratch
    // directory for this process, and take the keys this file writes out of anybody else's reach.
    let scratch = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-retry-policy-{}-{}",
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
            "# Written by retry_policy.rs. Nothing here is the operator's file.\n\
             [deepseek]\nkey = \"sk-test\"\n\
             url = \"http://127.0.0.1:{}/v1/chat/completions\"\n",
            endpoint.port
        ),
    )
    .expect("the config file");

    let first = config(&scratch, "probe", 1);
    let parts = Parts::load(&first).expect("the byte vocabulary needs no model file");
    // The operator's per-project mode is not this test's business (`wired.rs`'s rule).
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // ── A. A transport failure is retried, and the turn survives ─────────────────────────
    //
    // The stub hangs up on the first call and answers on the second. This is the class a DNS
    // lookup failure lands in — the report the policy opens with — and the assertion that
    // matters is the stub's own count: the turn was tried before it was called a failure.
    endpoint.refuse(1, Refusal::HangUp);
    let before = endpoint.seen();
    let cfg = config(&scratch, "transport", 1);
    let rec = drive(&parts, cfg, |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |_| answers(hub) >= 1);
    });
    assert_eq!(
        endpoint.seen() - before,
        2,
        "a call that never reached an answer was not tried again"
    );
    assert!(
        rec.failed.is_empty(),
        "the second call answered, so nothing failed: {:?}",
        rec.failed
    );

    // ── B. A 503 is retried ─────────────────────────────────────────────────────────────
    //
    // The status a server reloading a model answers, which is the whole reason the ladder is
    // longer than one wait.
    endpoint.refuse(1, Refusal::Unavailable);
    let before = endpoint.seen();
    let cfg = config(&scratch, "unavailable", 1);
    let rec = drive(&parts, cfg, |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |_| answers(hub) >= 1);
    });
    assert_eq!(
        endpoint.seen() - before,
        2,
        "a 503 was not waited out: the ladder exists for this status"
    );
    assert!(rec.failed.is_empty(), "{:?}", rec.failed);

    // ── C. A 401 is not retried, and reads as itself ─────────────────────────────────────
    endpoint.refuse(100, Refusal::Unauthorized);
    let before = endpoint.seen();
    let cfg = config(&scratch, "unauthorized", 5);
    let rec = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| !rec.failed.is_empty());
    });
    assert_eq!(
        endpoint.seen() - before,
        1,
        "a credential the provider refused was asked for again"
    );
    let said = &rec.failed[0];
    assert!(
        said.starts_with("http 401: "),
        "the status reads as itself: {said}"
    );
    assert!(
        said.contains("a refusal is not retried (1 call, no waiting)"),
        "and the sentence says there were no attempts: {said}"
    );

    // ── D. A 429 that names money is not retried: the six-times case ─────────────────────
    //
    // This is the test that would have seen six calls on the build before the policy: the status
    // alone says *not yet*, and only the body says *not ever until somebody tops up*.
    endpoint.refuse(100, Refusal::NoCredit);
    let before = endpoint.seen();
    let cfg = config(&scratch, "no-credit", 5);
    let rec = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| !rec.failed.is_empty());
    });
    assert_eq!(
        endpoint.seen() - before,
        1,
        "a billing refusal was taken again: no retry can top up an account"
    );
    let said = &rec.failed[0];
    assert!(said.starts_with("http 429: "), "{said}");
    assert!(
        said.contains("cannot pay"),
        "the sentence says what the body said: {said}"
    );

    // ── E. The ladder runs out, and the sentence names the attempts ──────────────────────
    //
    // One retry: two calls, one second of waiting, and the ending says both — which is the fact
    // `FAILED — http io: …` never carried.
    endpoint.refuse(100, Refusal::Unavailable);
    let before = endpoint.seen();
    let cfg = config(&scratch, "gives-up", 1);
    let rec = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| !rec.failed.is_empty());
    });
    assert_eq!(
        endpoint.seen() - before,
        2,
        "the ladder did not take the round twice, so this scenario measured nothing"
    );
    let said = &rec.failed[0];
    assert!(
        said.starts_with("tried 2 times over 1s, then gave up: "),
        "the ending names the attempts and the waiting: {said}"
    );
    assert!(
        said.contains("the provider said to try again"),
        "and the class, in words: {said}"
    );
    assert!(
        said.contains(UNAVAILABLE),
        "and the provider's own sentence, for the bug report: {said}"
    );
}
